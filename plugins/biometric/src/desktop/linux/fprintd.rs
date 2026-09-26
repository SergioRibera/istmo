//! The fingerprint reader behind fprintd (`net.reactivated.Fprint`) on
//! the system D-Bus.

use futures_lite::StreamExt;
use istmo_core::CancelToken;
use zbus::Connection;
use zbus::zvariant::OwnedObjectPath;

use super::super::until_cancelled;
use crate::{AuthMethod, BiometricError, BiometricStatus};

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
pub(super) enum FprintFailure {
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
    pub(super) const fn status(&self) -> BiometricStatus {
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

/// The default reader, acting for one user.
pub(super) struct Reader {
    device: DeviceProxy<'static>,
    user: String,
}

impl Reader {
    /// Connect to fprintd's default device, acting for `user` (`None`:
    /// the calling user).
    pub(super) async fn open(user: Option<&str>) -> Result<Self, FprintFailure> {
        let connection = Connection::system().await?;
        let manager = ManagerProxy::new(&connection).await?;
        let path = manager.get_default_device().await?;
        let device = DeviceProxy::builder(&connection)
            .path(path)?
            .build()
            .await?;
        Ok(Self {
            device,
            user: user.unwrap_or(CURRENT_USER).to_owned(),
        })
    }

    pub(super) async fn enrolled_fingers(&self) -> Result<Vec<String>, FprintFailure> {
        Ok(self.device.list_enrolled_fingers(&self.user).await?)
    }

    /// Claim the reader, run one verification and release it again.
    pub(super) async fn verify(&self, cancel: &CancelToken) -> Result<AuthMethod, BiometricError> {
        self.device
            .claim(&self.user)
            .await
            .map_err(FprintFailure::from)?;
        let outcome = self.scan(cancel).await;
        // Releasing only fails once the reader vanished; fprintd drops
        // the claim together with the bus connection anyway.
        if let Err(err) = self.device.release().await {
            tracing::debug!(%err, "fprintd Release failed");
        }
        outcome
    }

    /// Run one `VerifyStart` … `VerifyStop` cycle on the claimed device.
    async fn scan(&self, cancel: &CancelToken) -> Result<AuthMethod, BiometricError> {
        // Subscribe before starting so no status can slip through.
        let mut statuses = self
            .device
            .receive_verify_status()
            .await
            .map_err(FprintFailure::from)?;
        self.device
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
        let _ = self.device.verify_stop().await;
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
