//! Reference desktop backends for the biometric plugin.
//!
//! [`DesktopBiometric`] implements [`Biometric`] in Rust on top of each
//! desktop OS's user-verification API:
//!
//! * **macOS** — `LocalAuthentication` (`LAContext`), i.e. Touch ID with
//!   the login password as device credential. Secrets live in the
//!   data-protection keychain, which only signed apps carrying the
//!   `keychain-access-groups` entitlement may use; elsewhere they fail
//!   with [`BiometricError::UnsupportedOperation`].
//! * **Windows** — Windows Hello through `WinRT` `UserConsentVerifier`.
//!   Hello does not report which factor verified the user and always
//!   offers its PIN, so every policy behaves like
//!   [`AuthPolicy::BiometricOrDeviceCredential`] and success is reported
//!   as [`AuthMethod::Unspecified`]. Secrets are sealed with a key
//!   derived from a Hello credential signature and stored under
//!   `%LOCALAPPDATA%\istmo\biometric\<exe name>\`; storing the first
//!   secret of an alias shows Hello twice (create the key, then sign).
//!   Sensor kinds come from the Windows Biometric Framework; Hello
//!   exposes no enrollment state.
//! * **Linux** — `fprintd` over the system D-Bus. fprintd has no UI:
//!   the app must tell the user to touch the sensor while
//!   [`Biometric::authenticate`] is pending. There is no device
//!   credential fallback. Secrets are unsupported unless the app opts
//!   into [`DesktopBiometric::with_ui_gated_vault`].
//!
//! Register it as a Rust-hosted plugin:
//!
//! ```ignore
//! let backend = DesktopBiometric::default();
//! runtime.register_host(BiometricHost::new(backend));
//! ```

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::task::Poll;

use istmo_core::CancelToken;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::{
    AuthMethod, AuthPolicy, AuthPrompt, Availability, Biometric, BiometricError, SecretAlias,
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as platform;

/// Reference [`Biometric`] backend for desktop targets.
#[derive(Debug, Default, Clone)]
pub struct DesktopBiometric {
    options: DesktopOptions,
    backend: platform::Backend,
}

/// Knobs set through the [`DesktopBiometric`] builders. Each platform
/// reads the ones that apply to it.
#[derive(Debug, Default, Clone)]
struct DesktopOptions {
    /// Win32 `HWND` (as an address, to stay `Send + Sync`) that Windows
    /// Hello dialogs are brought in front of.
    parent_window: Option<isize>,
    /// fprintd user to verify; `None` is the caller's own user.
    fprintd_user: Option<String>,
    /// Linux: keep secrets in the Secret Service keyring behind an
    /// fprintd check.
    ui_gated_vault: bool,
}

impl DesktopBiometric {
    /// Anchor prompts to `window`.
    ///
    /// Windows uses it to parent the Windows Hello dialog — and to bring
    /// the key-credential dialogs of the vault to the foreground — so
    /// they open in front of the app instead of behind it. Ignored on
    /// macOS (the Touch ID sheet is system-modal) and Linux (fprintd has
    /// no UI).
    #[must_use]
    pub fn with_parent_window(mut self, window: &impl HasWindowHandle) -> Self {
        let raw = window.window_handle().map(|handle| handle.as_raw());
        if let Ok(RawWindowHandle::Win32(win32)) = raw {
            self.options.parent_window = Some(win32.hwnd.get());
        }
        self
    }

    /// Linux: verify `user`'s fingerprints instead of the calling user's.
    /// fprintd's polkit policy only lets privileged callers act for
    /// another user. Ignored elsewhere.
    #[must_use]
    pub fn with_fprintd_user(mut self, user: impl Into<String>) -> Self {
        self.options.fprintd_user = Some(user.into());
        self
    }

    /// Linux: enable the vault by storing secrets in the Secret Service
    /// keyring (GNOME Keyring, `KWallet`, …) and requiring an fprintd
    /// verification before every store and read.
    ///
    /// This is a **UI gate, not a cryptographic binding**: the secret is
    /// only as safe as the user's unlocked keyring, and any process of
    /// the same user can read it without a fingerprint. Opt in only when
    /// that is acceptable. Ignored on the other desktops, whose vaults
    /// are hardware- or OS-bound already.
    #[must_use]
    pub const fn with_ui_gated_vault(mut self) -> Self {
        self.options.ui_gated_vault = true;
        self
    }
}

impl Biometric for DesktopBiometric {
    async fn availability(&self, policy: AuthPolicy) -> Result<Availability, BiometricError> {
        self.backend.availability(&self.options, policy).await
    }

    async fn authenticate(
        &self,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<AuthMethod, BiometricError> {
        prompt.check()?;
        self.backend
            .authenticate(&self.options, prompt, cancel)
            .await
    }

    async fn store_secret(
        &self,
        alias: String,
        secret: Vec<u8>,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<(), BiometricError> {
        let alias = SecretAlias::try_from(alias)?;
        prompt.check()?;
        self.backend
            .store_secret(&self.options, &alias, secret, prompt, cancel)
            .await
    }

    async fn read_secret(
        &self,
        alias: String,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<Vec<u8>, BiometricError> {
        let alias = SecretAlias::try_from(alias)?;
        prompt.check()?;
        self.backend
            .read_secret(&self.options, &alias, prompt, cancel)
            .await
    }

    async fn delete_secret(&self, alias: String) -> Result<(), BiometricError> {
        let alias = SecretAlias::try_from(alias)?;
        self.backend.delete_secret(&self.options, &alias).await
    }

    async fn has_secret(&self, alias: String) -> Result<bool, BiometricError> {
        let alias = SecretAlias::try_from(alias)?;
        self.backend.has_secret(&self.options, &alias).await
    }

    async fn enrollment_state(&self) -> Result<Option<Vec<u8>>, BiometricError> {
        self.backend.enrollment_state(&self.options).await
    }
}

impl AuthPrompt {
    /// Reject prompts every desktop platform would refuse to show.
    fn check(&self) -> Result<(), BiometricError> {
        if self.reason.trim().is_empty() {
            return Err(BiometricError::InvalidPrompt(
                "`reason` must not be empty".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Drive `fut` to completion unless `cancel` fires first, in which case
/// `None` is returned and `fut` is dropped.
async fn until_cancelled<F: Future>(fut: F, cancel: &CancelToken) -> Option<F::Output> {
    let mut fut = pin!(fut);
    let mut cancelled = pin!(cancel.cancelled());
    poll_fn(|cx| {
        if let Poll::Ready(output) = fut.as_mut().poll(cx) {
            return Poll::Ready(Some(output));
        }
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        Poll::Pending
    })
    .await
}

#[cfg(any(target_os = "windows", target_os = "linux"))]
/// Namespace keeping apps apart in per-user stores shared by every
/// process (Windows Hello keys, the Secret Service keyring): the
/// executable's file stem, restricted to alias-safe characters.
fn app_namespace() -> String {
    let sanitize = |c: char| {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') {
            c
        } else {
            '_'
        }
    };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .map_or_else(
            || "app".to_owned(),
            |stem| stem.chars().map(sanitize).collect(),
        )
}
