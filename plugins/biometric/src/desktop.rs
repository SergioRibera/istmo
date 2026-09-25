//! Reference desktop backends for the biometric plugin.
//!
//! [`DesktopBiometric`] implements [`Biometric`] in Rust on top of each
//! desktop OS's user-verification API:
//!
//! * **macOS** — `LocalAuthentication` (`LAContext`), i.e. Touch ID with
//!   the login password as device credential.
//! * **Windows** — Windows Hello through `WinRT` `UserConsentVerifier`.
//!   Hello does not report which factor verified the user and always
//!   offers its PIN, so every policy behaves like
//!   [`AuthPolicy::BiometricOrDeviceCredential`] and success is reported
//!   as [`AuthMethod::Unspecified`].
//! * **Linux** — `fprintd` over the system D-Bus. fprintd has no UI:
//!   the app must tell the user to touch the sensor while
//!   [`Biometric::authenticate`] is pending. There is no device
//!   credential fallback.
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
use raw_window_handle::HasWindowHandle;

use crate::{AuthMethod, AuthPolicy, AuthPrompt, Availability, Biometric, BiometricError};

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
    backend: platform::Backend,
}

impl DesktopBiometric {
    /// Anchor prompts to `window`.
    ///
    /// Windows uses it to parent the Windows Hello dialog so it opens in
    /// front of the app instead of behind it. Ignored on macOS (the
    /// Touch ID sheet is system-modal) and Linux (fprintd has no UI).
    #[must_use]
    pub fn with_parent_window(mut self, window: &impl HasWindowHandle) -> Self {
        if let Ok(handle) = window.window_handle() {
            self.backend.set_parent_window(handle.as_raw());
        }
        self
    }
}

impl Biometric for DesktopBiometric {
    async fn availability(&self, policy: AuthPolicy) -> Result<Availability, BiometricError> {
        self.backend.availability(policy).await
    }

    async fn authenticate(
        &self,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<AuthMethod, BiometricError> {
        if prompt.reason.trim().is_empty() {
            return Err(BiometricError::InvalidPrompt(
                "`reason` must not be empty".to_owned(),
            ));
        }
        self.backend.authenticate(prompt, cancel).await
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
