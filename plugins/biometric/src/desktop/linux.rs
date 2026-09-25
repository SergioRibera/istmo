//! Linux backend: fprintd for verification, plus the opt-in UI-gated
//! Secret Service vault.

mod fprintd;
mod keyring;

use istmo_core::CancelToken;

use super::DesktopOptions;
use crate::{
    AuthMethod, AuthPolicy, AuthPrompt, Availability, BiometricError, BiometricKind,
    BiometricStatus, SecretAlias,
};
use fprintd::{FprintFailure, Reader};
use keyring::Keyring;

const NO_SECRETS: &str = "fprintd exposes no key material to bind secrets to; \
     opt into the keyring vault with `DesktopBiometric::with_ui_gated_vault`";

#[derive(Debug, Default, Clone)]
pub(super) struct Backend;

impl Backend {
    #[allow(clippy::unused_self)] // Uniform surface across platform backends.
    pub(super) async fn availability(
        &self,
        options: &DesktopOptions,
        _policy: AuthPolicy,
    ) -> Result<Availability, BiometricError> {
        let fingers = match Reader::open(options.fprintd_user.as_deref()).await {
            Ok(reader) => reader.enrolled_fingers().await,
            Err(failure) => Err(failure),
        };
        let status = match fingers {
            Ok(fingers) if fingers.is_empty() => BiometricStatus::NoneEnrolled,
            Ok(_) => BiometricStatus::Available,
            Err(failure) => failure.status(),
        };
        let kinds = match status {
            BiometricStatus::Unsupported | BiometricStatus::NoHardware => Vec::new(),
            _ => vec![BiometricKind::Fingerprint],
        };
        let vault_available = options.ui_gated_vault
            && status == BiometricStatus::Available
            && Keyring::open().await.is_ok();
        Ok(Availability {
            status,
            kinds,
            device_credential_available: false,
            vault_available,
        })
    }

    #[allow(clippy::unused_self)] // Uniform surface across platform backends.
    pub(super) async fn authenticate(
        &self,
        options: &DesktopOptions,
        _prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<AuthMethod, BiometricError> {
        let reader = Reader::open(options.fprintd_user.as_deref()).await?;
        reader.verify(&cancel).await
    }

    pub(super) async fn store_secret(
        &self,
        options: &DesktopOptions,
        alias: &SecretAlias,
        secret: Vec<u8>,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<(), BiometricError> {
        let keyring = Self::vault(options).await?;
        self.authenticate(options, prompt, cancel).await?;
        keyring.store(alias, secret).await
    }

    pub(super) async fn read_secret(
        &self,
        options: &DesktopOptions,
        alias: &SecretAlias,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<Vec<u8>, BiometricError> {
        let keyring = Self::vault(options).await?;
        if !keyring.contains(alias).await? {
            return Err(BiometricError::SecretNotFound);
        }
        self.authenticate(options, prompt, cancel).await?;
        keyring.read(alias).await
    }

    /// Without the vault nothing can have been stored, so there is
    /// nothing to delete.
    #[allow(clippy::unused_self)] // Uniform surface across platform backends.
    pub(super) async fn delete_secret(
        &self,
        options: &DesktopOptions,
        alias: &SecretAlias,
    ) -> Result<(), BiometricError> {
        if !options.ui_gated_vault {
            return Ok(());
        }
        Keyring::open().await?.delete(alias).await
    }

    #[allow(clippy::unused_self)] // Uniform surface across platform backends.
    pub(super) async fn has_secret(
        &self,
        options: &DesktopOptions,
        alias: &SecretAlias,
    ) -> Result<bool, BiometricError> {
        if !options.ui_gated_vault {
            return Ok(false);
        }
        Keyring::open().await?.contains(alias).await
    }

    /// The enrolled finger names, which change when fingers are added or
    /// removed (re-enrolling the same finger goes unnoticed).
    #[allow(clippy::unused_self)] // Uniform surface across platform backends.
    pub(super) async fn enrollment_state(
        &self,
        options: &DesktopOptions,
    ) -> Result<Option<Vec<u8>>, BiometricError> {
        let fingers = match Reader::open(options.fprintd_user.as_deref()).await {
            Ok(reader) => reader.enrolled_fingers().await,
            Err(failure) => Err(failure),
        };
        match fingers {
            Ok(fingers) => Ok(enrollment_token(fingers)),
            Err(
                FprintFailure::ServiceMissing
                | FprintFailure::NoDevice
                | FprintFailure::NoEnrolledPrints,
            ) => Ok(None),
            Err(failure) => Err(failure.into()),
        }
    }

    async fn vault(options: &DesktopOptions) -> Result<Keyring, BiometricError> {
        if !options.ui_gated_vault {
            return Err(BiometricError::UnsupportedOperation(NO_SECRETS.to_owned()));
        }
        Keyring::open().await
    }
}

/// Order-independent token over the enrolled finger names.
fn enrollment_token(mut fingers: Vec<String>) -> Option<Vec<u8>> {
    if fingers.is_empty() {
        return None;
    }
    fingers.sort_unstable();
    Some(fingers.join("\n").into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_token_ignores_finger_order() {
        let a = enrollment_token(vec!["right-index-finger".into(), "left-thumb".into()]);
        let b = enrollment_token(vec!["left-thumb".into(), "right-index-finger".into()]);
        assert_eq!(a, b);
        assert_ne!(a, enrollment_token(vec!["left-thumb".into()]));
        assert_eq!(enrollment_token(Vec::new()), None);
    }
}
