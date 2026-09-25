//! fprintd backend (`net.reactivated.Fprint`) over the system D-Bus.

use futures_lite::StreamExt;
use istmo_core::CancelToken;
use raw_window_handle::RawWindowHandle;
use zbus::Connection;
use zbus::zvariant::OwnedObjectPath;

use super::until_cancelled;
use crate::{
    AuthMethod, AuthPolicy, AuthPrompt, Availability, BiometricError, BiometricKind,
    BiometricStatus, SecretAlias,
};

const NO_SECRETS: &str = "fprintd exposes no key material to bind secrets to";

/// fprintd resolves an empty user name to the calling user.
const CURRENT_USER: &str = "";
/// Let fprintd pick whichever enrolled finger matches.
const ANY_FINGER: &str = "any";

#[zbus::proxy(
    interface = "net.reactivated.Fprint.Manager",
    default_service = "net.reactivated.Fprint",
    default_path = "/net/reactivated/Fprint/Manager",
    gen_blocking = false
)]
trait Manager {
    fn get_default_device(&self) -> zbus::Result<OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "net.reactivated.Fprint.Device",
    default_service = "net.reactivated.Fprint",
    gen_blocking = false
)]
trait Device {
    fn list_enrolled_fingers(&self, username: &str) -> zbus::Result<Vec<String>>;
    fn claim(&self, username: &str) -> zbus::Result<()>;
    fn release(&self) -> zbus::Result<()>;
    fn verify_start(&self, finger_name: &str) -> zbus::Result<()>;
    fn verify_stop(&self) -> zbus::Result<()>;

    #[zbus(signal)]
    fn verify_status(&self, result: &str, done: bool) -> zbus::Result<()>;
}

/// Failure modes of an fprintd round-trip, classified from the D-Bus
/// error so they can be mapped to both [`BiometricStatus`] and
/// [`BiometricError`].
#[derive(Debug)]
enum FprintFailure {
    /// No system bus, or fprintd is not installed.
    ServiceMissing,
    NoDevice,
    NoEnrolledPrints,
    /// Another client claimed the reader.
    Busy,
    /// polkit refused `net.reactivated.fprint.device.verify`.
    PermissionDenied(String),
    Other(String),
}

impl From<zbus::Error> for FprintFailure {
    fn from(err: zbus::Error) -> Self {
        let zbus::Error::MethodError(name, description, _) = &err else {
            return match err {
                zbus::Error::InputOutput(_) | zbus::Error::Address(_) => Self::ServiceMissing,
                other => Self::Other(other.to_string()),
            };
        };
        let detail = description.clone().unwrap_or_else(|| name.to_string());
        match name.as_str() {
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner"
            | "org.freedesktop.DBus.Error.Spawn.ServiceNotFound" => Self::ServiceMissing,
            "net.reactivated.Fprint.Error.NoSuchDevice" => Self::NoDevice,
            "net.reactivated.Fprint.Error.NoEnrolledPrints" => Self::NoEnrolledPrints,
            "net.reactivated.Fprint.Error.AlreadyInUse" => Self::Busy,
            "net.reactivated.Fprint.Error.PermissionDenied" => Self::PermissionDenied(detail),
            _ => Self::Other(detail),
        }
    }
}

impl FprintFailure {
    const fn status(&self) -> BiometricStatus {
        match self {
            Self::ServiceMissing => BiometricStatus::Unsupported,
            Self::NoDevice => BiometricStatus::NoHardware,
            Self::NoEnrolledPrints => BiometricStatus::NoneEnrolled,
            Self::Busy | Self::PermissionDenied(_) | Self::Other(_) => {
                BiometricStatus::HardwareUnavailable
            }
        }
    }
}

impl From<FprintFailure> for BiometricError {
    fn from(failure: FprintFailure) -> Self {
        match failure {
            FprintFailure::PermissionDenied(detail) => {
                Self::Backend(format!("fprintd permission denied: {detail}"))
            }
            FprintFailure::Other(detail) => Self::Backend(format!("fprintd: {detail}")),
            other => Self::NotAvailable(other.status()),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub(super) struct Backend;

impl Backend {
    // Mirrors the Windows backend, the only one that uses the window.
    #[allow(clippy::unused_self, clippy::needless_pass_by_ref_mut)]
    pub(super) const fn set_parent_window(&mut self, _window: RawWindowHandle) {}

    pub(super) async fn availability(
        &self,
        _policy: AuthPolicy,
    ) -> Result<Availability, BiometricError> {
        let status = match Self::enrolled_fingers().await {
            Ok(fingers) if fingers.is_empty() => BiometricStatus::NoneEnrolled,
            Ok(_) => BiometricStatus::Available,
            Err(failure) => failure.status(),
        };
        let kinds = match status {
            BiometricStatus::Unsupported | BiometricStatus::NoHardware => Vec::new(),
            _ => vec![BiometricKind::Fingerprint],
        };
        Ok(Availability {
            status,
            kinds,
            device_credential_available: false,
        })
    }

    pub(super) async fn authenticate(
        &self,
        _prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<AuthMethod, BiometricError> {
        let device = Self::default_device().await?;
        device
            .claim(CURRENT_USER)
            .await
            .map_err(FprintFailure::from)?;
        let outcome = Self::verify(&device, &cancel).await;
        // Releasing only fails once the reader vanished; fprintd drops
        // the claim together with the bus connection anyway.
        if let Err(err) = device.release().await {
            tracing::debug!(%err, "fprintd Release failed");
        }
        outcome
    }

    #[allow(clippy::unused_async)] // Async like every platform backend.
    pub(super) async fn store_secret(
        &self,
        _alias: &SecretAlias,
        _secret: Vec<u8>,
        _prompt: AuthPrompt,
        _cancel: CancelToken,
    ) -> Result<(), BiometricError> {
        Err(BiometricError::UnsupportedOperation(NO_SECRETS.to_owned()))
    }

    #[allow(clippy::unused_async)] // Async like every platform backend.
    pub(super) async fn read_secret(
        &self,
        _alias: &SecretAlias,
        _prompt: AuthPrompt,
        _cancel: CancelToken,
    ) -> Result<Vec<u8>, BiometricError> {
        Err(BiometricError::UnsupportedOperation(NO_SECRETS.to_owned()))
    }

    /// Nothing can be stored, so there is nothing to delete.
    #[allow(clippy::unused_async)] // Async like every platform backend.
    pub(super) async fn delete_secret(&self, _alias: &SecretAlias) -> Result<(), BiometricError> {
        Ok(())
    }

    #[allow(clippy::unused_async)] // Async like every platform backend.
    pub(super) async fn has_secret(&self, _alias: &SecretAlias) -> Result<bool, BiometricError> {
        Ok(false)
    }

    async fn default_device() -> Result<DeviceProxy<'static>, FprintFailure> {
        let connection = Connection::system().await?;
        let manager = ManagerProxy::new(&connection).await?;
        let path = manager.get_default_device().await?;
        Ok(DeviceProxy::builder(&connection)
            .path(path)?
            .build()
            .await?)
    }

    async fn enrolled_fingers() -> Result<Vec<String>, FprintFailure> {
        let device = Self::default_device().await?;
        Ok(device.list_enrolled_fingers(CURRENT_USER).await?)
    }

    /// Run one `VerifyStart` … `VerifyStop` cycle on a claimed device.
    async fn verify(
        device: &DeviceProxy<'static>,
        cancel: &CancelToken,
    ) -> Result<AuthMethod, BiometricError> {
        // Subscribe before starting so no status can slip through.
        let mut statuses = device
            .receive_verify_status()
            .await
            .map_err(FprintFailure::from)?;
        device
            .verify_start(ANY_FINGER)
            .await
            .map_err(FprintFailure::from)?;

        let scan = async {
            while let Some(signal) = statuses.next().await {
                let args = signal
                    .args()
                    .map_err(|err| BiometricError::Backend(err.to_string()))?;
                if let Some(outcome) = VerifyResult::from(args.result).outcome(args.done) {
                    return outcome;
                }
            }
            Err(BiometricError::Backend(
                "fprintd closed the VerifyStatus stream".to_owned(),
            ))
        };
        let outcome = until_cancelled(scan, cancel)
            .await
            .unwrap_or(Err(BiometricError::SystemCancelled));
        // Stopping an already finished verification is harmless.
        let _ = device.verify_stop().await;
        outcome
    }
}

/// `result` argument of the `VerifyStatus` signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerifyResult {
    Match,
    NoMatch,
    /// Bad scan (swipe too short, finger not centred, …); the user may
    /// try again.
    Retry,
    Disconnected,
    UnknownError,
}

impl From<&str> for VerifyResult {
    fn from(result: &str) -> Self {
        match result {
            "verify-match" => Self::Match,
            "verify-no-match" => Self::NoMatch,
            "verify-disconnected" => Self::Disconnected,
            "verify-retry-scan"
            | "verify-swipe-too-short"
            | "verify-finger-not-centered"
            | "verify-remove-and-retry" => Self::Retry,
            _ => Self::UnknownError,
        }
    }
}

impl VerifyResult {
    /// Terminal outcome of the verification, or `None` while fprintd
    /// keeps scanning.
    fn outcome(self, done: bool) -> Option<Result<AuthMethod, BiometricError>> {
        match (self, done) {
            (Self::Match, _) => Some(Ok(AuthMethod::Biometric)),
            (Self::Disconnected, _) => Some(Err(BiometricError::NotAvailable(
                BiometricStatus::HardwareUnavailable,
            ))),
            (Self::UnknownError, true) => Some(Err(BiometricError::Backend(
                "fprintd reported an unknown verification error".to_owned(),
            ))),
            (Self::NoMatch | Self::Retry, true) => Some(Err(BiometricError::AuthFailed)),
            (Self::NoMatch | Self::Retry | Self::UnknownError, false) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_results_map_to_terminal_outcomes() {
        assert_eq!(
            VerifyResult::from("verify-match").outcome(true),
            Some(Ok(AuthMethod::Biometric))
        );
        assert_eq!(
            VerifyResult::from("verify-no-match").outcome(true),
            Some(Err(BiometricError::AuthFailed))
        );
        assert_eq!(VerifyResult::from("verify-retry-scan").outcome(false), None);
        assert_eq!(
            VerifyResult::from("verify-disconnected").outcome(false),
            Some(Err(BiometricError::NotAvailable(
                BiometricStatus::HardwareUnavailable
            )))
        );
    }

    #[test]
    fn missing_service_is_reported_as_unsupported() {
        assert_eq!(
            FprintFailure::ServiceMissing.status(),
            BiometricStatus::Unsupported
        );
        assert_eq!(
            BiometricError::from(FprintFailure::NoEnrolledPrints),
            BiometricError::NotAvailable(BiometricStatus::NoneEnrolled)
        );
    }
}
