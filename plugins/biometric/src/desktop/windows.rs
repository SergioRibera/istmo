//! Windows Hello backend through `WinRT` `UserConsentVerifier`.

use istmo_core::CancelToken;
use raw_window_handle::RawWindowHandle;
use windows::Security::Credentials::UI::{
    UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
use windows::core::{HSTRING, factory};
use windows_future::IAsyncOperation;

use super::until_cancelled;
use crate::{AuthMethod, AuthPolicy, AuthPrompt, Availability, BiometricError, BiometricStatus};

#[derive(Debug, Default, Clone)]
pub(super) struct Backend {
    /// `HWND` the Hello dialog is parented to, as an address so the
    /// backend stays `Send + Sync`.
    parent_window: Option<isize>,
}

impl Backend {
    pub(super) const fn set_parent_window(&mut self, window: RawWindowHandle) {
        if let RawWindowHandle::Win32(handle) = window {
            self.parent_window = Some(handle.hwnd.get());
        }
    }

    pub(super) async fn availability(
        &self,
        _policy: AuthPolicy,
    ) -> Result<Availability, BiometricError> {
        let availability = UserConsentVerifier::CheckAvailabilityAsync()
            .map_err(winrt_error)?
            .await
            .map_err(winrt_error)?;
        let status = HelloAvailability(availability).status();
        Ok(Availability {
            status,
            // Hello does not say which sensors back it.
            kinds: Vec::new(),
            // A configured Hello always includes its PIN.
            device_credential_available: status == BiometricStatus::Available,
        })
    }

    pub(super) async fn authenticate(
        &self,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<AuthMethod, BiometricError> {
        let operation = self.request_verification(&HSTRING::from(prompt.reason.as_str()))?;
        let Some(result) = until_cancelled(operation.clone().into_future(), &cancel).await else {
            // Best effort: Hello may keep its dialog up regardless.
            let _ = operation.Cancel();
            return Err(BiometricError::SystemCancelled);
        };
        HelloResult(result.map_err(winrt_error)?).into()
    }

    fn request_verification(
        &self,
        message: &HSTRING,
    ) -> Result<IAsyncOperation<UserConsentVerificationResult>, BiometricError> {
        let Some(hwnd) = self.parent_window else {
            return UserConsentVerifier::RequestVerificationAsync(message).map_err(winrt_error);
        };
        let interop =
            factory::<UserConsentVerifier, IUserConsentVerifierInterop>().map_err(winrt_error)?;
        // SAFETY: `hwnd` came from a live `Win32WindowHandle` handed to
        // `DesktopBiometric::with_parent_window`; the caller keeps that
        // window alive while it authenticates.
        unsafe { interop.RequestVerificationForWindowAsync(HWND(hwnd as *mut _), message) }
            .map_err(winrt_error)
    }
}

#[allow(clippy::needless_pass_by_value)] // Used as `map_err(winrt_error)`.
fn winrt_error(err: windows::core::Error) -> BiometricError {
    BiometricError::Backend(format!("Windows Hello: {err}"))
}

struct HelloAvailability(UserConsentVerifierAvailability);

impl HelloAvailability {
    const fn status(&self) -> BiometricStatus {
        match self.0 {
            UserConsentVerifierAvailability::Available => BiometricStatus::Available,
            UserConsentVerifierAvailability::DeviceNotPresent => BiometricStatus::NoHardware,
            UserConsentVerifierAvailability::NotConfiguredForUser => BiometricStatus::NoneEnrolled,
            _ => BiometricStatus::HardwareUnavailable,
        }
    }
}

struct HelloResult(UserConsentVerificationResult);

impl From<HelloResult> for Result<AuthMethod, BiometricError> {
    fn from(result: HelloResult) -> Self {
        match result.0 {
            UserConsentVerificationResult::Verified => Ok(AuthMethod::Unspecified),
            UserConsentVerificationResult::Canceled => Err(BiometricError::UserCancelled),
            UserConsentVerificationResult::RetriesExhausted => Err(BiometricError::AuthFailed),
            UserConsentVerificationResult::DeviceNotPresent => {
                Err(BiometricError::NotAvailable(BiometricStatus::NoHardware))
            }
            UserConsentVerificationResult::NotConfiguredForUser => {
                Err(BiometricError::NotAvailable(BiometricStatus::NoneEnrolled))
            }
            UserConsentVerificationResult::DisabledByPolicy
            | UserConsentVerificationResult::DeviceBusy => Err(BiometricError::NotAvailable(
                BiometricStatus::HardwareUnavailable,
            )),
            other => Err(BiometricError::Backend(format!(
                "Windows Hello returned result {}",
                other.0
            ))),
        }
    }
}
