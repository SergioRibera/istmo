//! Secret vault sealed by a Windows Hello credential.
//!
//! Hello keys are RSA-2048 signing with PKCS#1 v1.5, which is
//! deterministic: signing a fixed challenge always yields the same
//! bytes, and Hello only signs after verifying the user. HKDF-SHA256
//! turns that signature into the AES-256-GCM key sealing the secret
//! file under `%LOCALAPPDATA%\istmo\biometric\<app>\<alias>.bin`.

use std::io::ErrorKind;
use std::path::PathBuf;

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use hkdf::Hkdf;
use istmo_core::CancelToken;
use sha2::{Digest, Sha256};
use windows::Security::Credentials::{
    KeyCredential, KeyCredentialCreationOption, KeyCredentialManager, KeyCredentialStatus,
};
use windows::Security::Cryptography::CryptographicBuffer;
use windows::core::{Array, HSTRING};

use super::{complete, winrt_error};
use crate::{BiometricError, BiometricStatus, SecretAlias};

/// Bumped whenever the derivation or the file layout changes.
const FORMAT_VERSION: u8 = 1;
const DOMAIN: &str = "istmo.biometric.v1";
const NONCE_LEN: usize = 12;

#[derive(Debug, Clone)]
pub(super) struct Vault {
    /// Keeps apps sharing the per-user Hello key store and
    /// `%LOCALAPPDATA%` apart: the executable's file stem.
    app: String,
}

impl Default for Vault {
    fn default() -> Self {
        let sanitize = |c: char| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') {
                c
            } else {
                '_'
            }
        };
        let app = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .map_or_else(
                || "app".to_owned(),
                |stem| stem.chars().map(sanitize).collect(),
            );
        Self { app }
    }
}

impl Vault {
    pub(super) async fn store(
        &self,
        alias: &SecretAlias,
        secret: &[u8],
        cancel: &CancelToken,
    ) -> Result<(), BiometricError> {
        Self::ensure_supported(cancel).await?;
        let name = self.credential_name(alias);
        let credential = Self::open_or_create(&name, cancel).await?;
        let cipher = Self::cipher(&credential, &name, cancel).await?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let sealed = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: secret,
                    aad: alias.as_str().as_bytes(),
                },
            )
            .map_err(|_| BiometricError::Backend("AES-GCM encryption failed".to_owned()))?;

        let mut file = Vec::with_capacity(1 + NONCE_LEN + sealed.len());
        file.push(FORMAT_VERSION);
        file.extend_from_slice(&nonce);
        file.extend_from_slice(&sealed);
        let path = self.path(alias)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io_error)?;
        }
        // Write-then-rename so a crash never leaves a torn secret.
        let staging = path.with_extension("tmp");
        std::fs::write(&staging, &file).map_err(io_error)?;
        std::fs::rename(&staging, &path).map_err(io_error)
    }

    pub(super) async fn read(
        &self,
        alias: &SecretAlias,
        cancel: &CancelToken,
    ) -> Result<Vec<u8>, BiometricError> {
        let path = self.path(alias)?;
        let file = match std::fs::read(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Err(BiometricError::SecretNotFound);
            }
            Err(err) => return Err(io_error(err)),
        };
        let Some((&FORMAT_VERSION, rest)) = file.split_first() else {
            return Err(BiometricError::Backend(
                "unrecognised vault file".to_owned(),
            ));
        };
        if rest.len() < NONCE_LEN {
            return Err(BiometricError::Backend("truncated vault file".to_owned()));
        }
        let (nonce, sealed) = rest.split_at(NONCE_LEN);

        let name = self.credential_name(alias);
        let opened = complete(
            KeyCredentialManager::OpenAsync(&name).map_err(winrt_error)?,
            cancel,
        )
        .await?;
        match opened.Status().map_err(winrt_error)? {
            KeyCredentialStatus::Success => {}
            // The Hello key is gone (PIN reset, Hello removed): the
            // secret can never be decrypted again.
            KeyCredentialStatus::NotFound => {
                self.discard(alias)?;
                return Err(BiometricError::KeyInvalidated);
            }
            other => return Err(HelloKeyStatus(other).into()),
        }
        let credential = opened.Credential().map_err(winrt_error)?;
        let cipher = Self::cipher(&credential, &name, cancel).await?;
        cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: sealed,
                    aad: alias.as_str().as_bytes(),
                },
            )
            .map_err(|_| BiometricError::KeyInvalidated)
    }

    pub(super) async fn delete(&self, alias: &SecretAlias) -> Result<(), BiometricError> {
        self.discard(alias)?;
        // Deleting a missing credential fails; nothing is left either way.
        if let Ok(action) = KeyCredentialManager::DeleteAsync(&self.credential_name(alias)) {
            let _ = action.await;
        }
        Ok(())
    }

    pub(super) async fn contains(&self, alias: &SecretAlias) -> Result<bool, BiometricError> {
        if !self.path(alias)?.is_file() {
            return Ok(false);
        }
        let opened = KeyCredentialManager::OpenAsync(&self.credential_name(alias))
            .map_err(winrt_error)?
            .await
            .map_err(winrt_error)?;
        Ok(opened.Status().map_err(winrt_error)? == KeyCredentialStatus::Success)
    }

    async fn ensure_supported(cancel: &CancelToken) -> Result<(), BiometricError> {
        let supported = complete(
            KeyCredentialManager::IsSupportedAsync().map_err(winrt_error)?,
            cancel,
        )
        .await?;
        if supported {
            Ok(())
        } else {
            Err(BiometricError::NotAvailable(BiometricStatus::NoneEnrolled))
        }
    }

    async fn open_or_create(
        name: &HSTRING,
        cancel: &CancelToken,
    ) -> Result<KeyCredential, BiometricError> {
        let opened = complete(
            KeyCredentialManager::OpenAsync(name).map_err(winrt_error)?,
            cancel,
        )
        .await?;
        let result = if opened.Status().map_err(winrt_error)? == KeyCredentialStatus::Success {
            opened
        } else {
            let create = KeyCredentialManager::RequestCreateAsync(
                name,
                KeyCredentialCreationOption::ReplaceExisting,
            )
            .map_err(winrt_error)?;
            complete(create, cancel).await?
        };
        match result.Status().map_err(winrt_error)? {
            KeyCredentialStatus::Success => result.Credential().map_err(winrt_error),
            other => Err(HelloKeyStatus(other).into()),
        }
    }

    /// Have Hello sign this credential's challenge — which verifies the
    /// user — and derive the AES key from the signature.
    async fn cipher(
        credential: &KeyCredential,
        name: &HSTRING,
        cancel: &CancelToken,
    ) -> Result<Aes256Gcm, BiometricError> {
        let challenge = Sha256::new()
            .chain_update(DOMAIN)
            .chain_update(b"/challenge/")
            .chain_update(name.to_string_lossy())
            .finalize();
        // `IBuffer` is not `Send`: keep it out of the awaiting scope.
        let request = {
            let buffer =
                CryptographicBuffer::CreateFromByteArray(&challenge).map_err(winrt_error)?;
            credential.RequestSignAsync(&buffer).map_err(winrt_error)?
        };
        let signed = complete(request, cancel).await?;
        match signed.Status().map_err(winrt_error)? {
            KeyCredentialStatus::Success => {}
            other => return Err(HelloKeyStatus(other).into()),
        }
        let mut signature = Array::<u8>::new();
        CryptographicBuffer::CopyToByteArray(
            &signed.Result().map_err(winrt_error)?,
            &mut signature,
        )
        .map_err(winrt_error)?;

        let mut key = Key::<Aes256Gcm>::default();
        Hkdf::<Sha256>::new(Some(DOMAIN.as_bytes()), &signature)
            .expand(name.to_string_lossy().as_bytes(), &mut key)
            .map_err(|_| BiometricError::Backend("HKDF expansion failed".to_owned()))?;
        Ok(Aes256Gcm::new(&key))
    }

    fn credential_name(&self, alias: &SecretAlias) -> HSTRING {
        HSTRING::from(format!("{DOMAIN}.{}.{alias}", self.app))
    }

    fn path(&self, alias: &SecretAlias) -> Result<PathBuf, BiometricError> {
        let base = std::env::var_os("LOCALAPPDATA")
            .ok_or_else(|| BiometricError::Backend("%LOCALAPPDATA% is not set".to_owned()))?;
        Ok(PathBuf::from(base)
            .join("istmo")
            .join("biometric")
            .join(&self.app)
            .join(format!("{alias}.bin")))
    }

    fn discard(&self, alias: &SecretAlias) -> Result<(), BiometricError> {
        match std::fs::remove_file(self.path(alias)?) {
            Err(err) if err.kind() != ErrorKind::NotFound => Err(io_error(err)),
            _ => Ok(()),
        }
    }
}

#[allow(clippy::needless_pass_by_value)] // Used as `map_err(io_error)`.
fn io_error(err: std::io::Error) -> BiometricError {
    BiometricError::Backend(format!("vault file: {err}"))
}

/// Non-success `KeyCredentialStatus`.
struct HelloKeyStatus(KeyCredentialStatus);

impl From<HelloKeyStatus> for BiometricError {
    fn from(status: HelloKeyStatus) -> Self {
        match status.0 {
            KeyCredentialStatus::UserCanceled => Self::UserCancelled,
            KeyCredentialStatus::UserPrefersPassword => Self::UserFallback,
            KeyCredentialStatus::SecurityDeviceLocked => Self::LockedOut,
            KeyCredentialStatus::NotFound => Self::SecretNotFound,
            other => Self::Backend(format!("Windows Hello key status {}", other.0)),
        }
    }
}
