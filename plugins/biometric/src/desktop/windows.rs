//! Windows Hello backend through `WinRT` `UserConsentVerifier`, plus a
//! Hello-sealed vault for biometric-bound secrets.

mod vault;

use istmo_core::CancelToken;
use windows::Security::Credentials::UI::{
    UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
};
use windows::Win32::Devices::BiometricFramework::{
    WINBIO_UNIT_SCHEMA, WinBioEnumBiometricUnits, WinBioFree,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow};
use windows::core::{HSTRING, RuntimeType, factory, w};
use windows_future::IAsyncOperation;

use super::{DesktopOptions, until_cancelled};
use crate::{
    AuthMethod, AuthPolicy, AuthPrompt, Availability, BiometricError, BiometricKind,
    BiometricStatus, SecretAlias,
};

/// `WINBIO_BIOMETRIC_TYPE` values from `<winbio_types.h>`, missing from
/// the `windows` crate.
const WINBIO_TYPE_FACIAL_FEATURES: u32 = 0x0000_0002;
const WINBIO_TYPE_FINGERPRINT: u32 = 0x0000_0008;
const WINBIO_TYPE_IRIS: u32 = 0x0000_0010;

#[derive(Debug, Default, Clone)]
pub(super) struct Backend {
    vault: vault::Vault,
}

impl Backend {
    pub(super) async fn availability(
        &self,
        _options: &DesktopOptions,
        _policy: AuthPolicy,
    ) -> Result<Availability, BiometricError> {
        let availability = UserConsentVerifier::CheckAvailabilityAsync()
            .map_err(winrt_error)?
            .await
            .map_err(winrt_error)?;
        let status = HelloAvailability(availability).status();
        let vault_available = status == BiometricStatus::Available
            && KeyCredentialSupport::check(&CancelToken::new()).await;
        Ok(Availability {
            status,
            kinds: sensor_kinds(),
            // A configured Hello always includes its PIN.
            device_credential_available: status == BiometricStatus::Available,
            vault_available,
        })
    }

    pub(super) async fn authenticate(
        &self,
        options: &DesktopOptions,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<AuthMethod, BiometricError> {
        let operation =
            Self::request_verification(options, &HSTRING::from(prompt.reason.as_str()))?;
        HelloResult(complete(operation, &cancel).await?).into()
    }

    /// Hello shows its own texts for key operations; `prompt` is only
    /// validated.
    pub(super) async fn store_secret(
        &self,
        options: &DesktopOptions,
        alias: &SecretAlias,
        secret: Vec<u8>,
        _prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<(), BiometricError> {
        self.vault
            .store(alias, &secret, options.parent_window, &cancel)
            .await
    }

    pub(super) async fn read_secret(
        &self,
        options: &DesktopOptions,
        alias: &SecretAlias,
        _prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<Vec<u8>, BiometricError> {
        self.vault.read(alias, options.parent_window, &cancel).await
    }

    pub(super) async fn delete_secret(
        &self,
        _options: &DesktopOptions,
        alias: &SecretAlias,
    ) -> Result<(), BiometricError> {
        self.vault.delete(alias).await
    }

    pub(super) async fn has_secret(
        &self,
        _options: &DesktopOptions,
        alias: &SecretAlias,
    ) -> Result<bool, BiometricError> {
        self.vault.contains(alias).await
    }

    /// Windows Hello exposes no view of its enrollment.
    #[allow(clippy::unused_async, clippy::unused_self)] // Uniform backend surface.
    pub(super) async fn enrollment_state(
        &self,
        _options: &DesktopOptions,
    ) -> Result<Option<Vec<u8>>, BiometricError> {
        Ok(None)
    }

    fn request_verification(
        options: &DesktopOptions,
        message: &HSTRING,
    ) -> Result<IAsyncOperation<UserConsentVerificationResult>, BiometricError> {
        let Some(hwnd) = options.parent_window else {
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

/// `KeyCredentialManager` (the vault's key store) support.
struct KeyCredentialSupport;

impl KeyCredentialSupport {
    async fn check(cancel: &CancelToken) -> bool {
        match windows::Security::Credentials::KeyCredentialManager::IsSupportedAsync() {
            Ok(operation) => complete(operation, cancel).await.unwrap_or(false),
            Err(_) => false,
        }
    }
}

/// Sensor types registered with the Windows Biometric Framework.
fn sensor_kinds() -> Vec<BiometricKind> {
    [
        (WINBIO_TYPE_FINGERPRINT, BiometricKind::Fingerprint),
        (WINBIO_TYPE_FACIAL_FEATURES, BiometricKind::Face),
        (WINBIO_TYPE_IRIS, BiometricKind::Iris),
    ]
    .into_iter()
    .filter(|(factor, _)| has_biometric_unit(*factor))
    .map(|(_, kind)| kind)
    .collect()
}

fn has_biometric_unit(factor: u32) -> bool {
    let mut units: *mut WINBIO_UNIT_SCHEMA = std::ptr::null_mut();
    let mut count = 0usize;
    // SAFETY: both out-pointers are valid for writes; on success WinBio
    // allocates `units`, which is released with `WinBioFree` below.
    let found = unsafe { WinBioEnumBiometricUnits(factor, &raw mut units, &raw mut count) }.is_ok()
        && count > 0;
    if !units.is_null() {
        // SAFETY: `units` was allocated by `WinBioEnumBiometricUnits`.
        let _ = unsafe { WinBioFree(units.cast_const().cast()) };
    }
    found
}

/// Key-credential dialogs cannot be parented to a window, so a desktop
/// app's dialog may open behind it. When the app registered a parent
/// window, look for the dialog for a few seconds and bring it forward.
fn raise_credential_dialog(parent_window: Option<isize>) {
    if parent_window.is_none() {
        return;
    }
    std::thread::spawn(|| {
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            // SAFETY: `FindWindowW` only reads the static class name.
            if let Ok(dialog) = unsafe { FindWindowW(w!("Credential Dialog Xaml Host"), None) } {
                // SAFETY: `dialog` is a window handle just returned by
                // the system; a stale handle only makes the call fail.
                let _ = unsafe { SetForegroundWindow(dialog) };
                return;
            }
        }
    });
}

/// Await a `WinRT` operation unless `cancel` fires first, in which case
/// the operation is cancelled (best effort: Hello may keep its dialog
/// up regardless).
async fn complete<T: RuntimeType + 'static>(
    operation: IAsyncOperation<T>,
    cancel: &CancelToken,
) -> Result<T, BiometricError> {
    let Some(result) = until_cancelled(operation.clone().into_future(), cancel).await else {
        let _ = operation.Cancel();
        return Err(BiometricError::SystemCancelled);
    };
    result.map_err(winrt_error)
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
