//! Cross-platform biometric authentication plugin for
//! [`istmo`](https://docs.rs/istmo).
//!
//! Rust code sees a single async [`Biometric`] trait; each platform
//! backs it with its native user-verification API:
//!
//! | Platform | Backend                                                    | Lives in               |
//! | -------- | ---------------------------------------------------------- | ---------------------- |
//! | Android  | `androidx.biometric` (`BiometricManager`, `BiometricPrompt`) | `native/android/`      |
//! | iOS      | `LocalAuthentication` (`LAContext`)                        | `native/ios/`          |
//! | macOS    | `LocalAuthentication` (Touch ID)                           | Rust                   |
//! | Windows  | Windows Hello (`UserConsentVerifier`)                      | Rust                   |
//! | Linux    | `fprintd` over the system D-Bus                            | Rust                   |
//!
//! # Checking availability
//!
//! [`Biometric::availability`] reports whether the device can satisfy an
//! [`AuthPolicy`] right now, which biometric sensors it exposes and
//! whether a device credential (PIN / pattern / password) is set up.
//! Call it before rendering a "unlock with Face ID" button — it never
//! shows UI.
//!
//! # Authenticating
//!
//! [`Biometric::authenticate`] presents the system prompt described by
//! an [`AuthPrompt`] and resolves with the [`AuthMethod`] the user
//! verified with. Dropping the returned future cancels the call and
//! dismisses the prompt (`BiometricPrompt::cancelAuthentication` on
//! Android, `LAContext::invalidate` on Apple platforms,
//! `VerifyStop` on fprintd). Windows Hello only offers a best-effort
//! `IAsyncInfo::Cancel`; its dialog may stay up until the user answers.
//!
//! A successful [`Biometric::authenticate`] is a *UI gate*: on a rooted
//! or jailbroken device an attacker can forge the success callback.
//! Protect real secrets with key material that the OS only releases
//! after verification.
//!
//! # Deployment
//!
//! Prompts belong to the foreground app process, so the plugin is
//! pinned to `default_deployment = "local"` in `istmo.toml`.
//!
//! # Registration
//!
//! Android's `BiometricPrompt` needs a `FragmentActivity`, so
//! auto-registration is disabled. Register the reference backends by
//! hand:
//!
//! ```kotlin
//! // Inside FragmentActivity.onCreate (GameActivity / AppCompatActivity)
//! IstmoRuntime.registerHandler(
//!     BiometricDispatcher.PLUGIN_ID,
//!     BiometricDispatcher(BiometricBackendImpl(this), BiometricCodecsImpl()),
//! )
//! ```
//!
//! ```swift
//! IstmoRuntime.shared.registerHandler(
//!     BiometricDispatcher.PLUGIN_ID,
//!     BiometricDispatcher(backend: BiometricBackendImpl(), codecs: BiometricCodecsImpl())
//! )
//! ```
//!
//! # iOS `Info.plist`
//!
//! iOS terminates any app that evaluates Face ID without an
//! `NSFaceIDUsageDescription`. The plugin declares a default through
//! `[plugin.info_plist]`; apps localise it with `[app.info_plist]` in
//! their own `istmo.toml`.

#![doc(html_root_url = "https://docs.rs/istmo-biometric")]

#[cfg(feature = "codegen")]
pub mod codegen;

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
pub mod desktop;

#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
pub use desktop::DesktopBiometric;

use istmo_core::{CancelToken, IstmoError, codec};
use istmo_macros::{message, plugin};

/// Wire identifier for the biometric plugin.
pub const BIOMETRIC_PLUGIN_ID: &str = "istmo.biometric";

/// Which authenticators may satisfy a request.
///
/// Platforms that cannot tell biometric classes apart treat
/// [`AuthPolicy::BiometricStrong`] and [`AuthPolicy::BiometricWeak`]
/// alike. Windows Hello always offers its PIN as a fallback, whatever
/// the policy.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AuthPolicy {
    /// Class 3 ("strong") biometrics only — the only class Android
    /// accepts for unlocking Keystore keys.
    #[default]
    BiometricStrong,
    /// Class 2 ("weak") biometrics or better, such as camera-based face
    /// unlock on Android devices without dedicated hardware.
    BiometricWeak,
    /// Strong biometrics, falling back to the device PIN / pattern /
    /// password.
    BiometricOrDeviceCredential,
}

/// Why a device can or cannot satisfy an [`AuthPolicy`].
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BiometricStatus {
    /// Authentication can be requested right now.
    Available,
    /// Hardware is present but the user has not enrolled a biometric
    /// (or, for [`AuthPolicy::BiometricOrDeviceCredential`], set up a
    /// device credential).
    NoneEnrolled,
    /// The device has no matching sensor.
    NoHardware,
    /// The sensor exists but is temporarily unusable (busy, disabled by
    /// policy, disconnected).
    HardwareUnavailable,
    /// Too many failed attempts; biometrics are locked until the user
    /// authenticates with the device credential.
    LockedOut,
    /// Android: a security vulnerability was found and the sensor is
    /// disabled until an OS update.
    SecurityUpdateRequired,
    /// The platform offers no biometric API at all.
    Unsupported,
}

/// Kind of biometric sensor exposed by the device.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BiometricKind {
    Fingerprint,
    Face,
    /// Iris or Optic ID.
    Iris,
    /// A sensor the platform does not identify further (Windows Hello,
    /// Android before API 29).
    Other,
}

/// Result of [`Biometric::availability`].
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Availability {
    /// Whether the requested [`AuthPolicy`] can be satisfied.
    pub status: BiometricStatus,
    /// Sensors the device exposes. Best effort — may be empty when the
    /// platform does not report sensor types.
    pub kinds: Vec<BiometricKind>,
    /// Whether a device PIN / pattern / password is set up.
    pub device_credential_available: bool,
}

impl Availability {
    /// `true` when [`Biometric::authenticate`] can be called with the
    /// policy this availability was computed for.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self.status, BiometricStatus::Available)
    }
}

/// Text and behaviour of the system prompt shown by
/// [`Biometric::authenticate`].
///
/// Every platform renders a different subset:
///
/// * Android → `title`, `subtitle`, `reason` (as description),
///   `cancel_label` (negative button), `confirmation_required`.
/// * iOS / macOS → `reason` (`localizedReason`), `cancel_label`,
///   `fallback_label`.
/// * Windows → `reason` (dialog message).
/// * Linux → nothing: fprintd has no UI, so the app must tell the user
///   to touch the sensor.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuthPrompt {
    /// Prompt title (Android).
    pub title: String,
    /// Why the app asks for authentication. Must not be empty — Apple
    /// rejects an empty `localizedReason`.
    pub reason: String,
    /// Secondary line under the title (Android).
    pub subtitle: Option<String>,
    /// Label of the cancel / negative button. Android requires one when
    /// the policy excludes the device credential; backends fall back to
    /// a localized "Cancel".
    pub cancel_label: Option<String>,
    /// Label of the "enter password" fallback button (Apple). `Some("")`
    /// hides the button; `None` keeps the system default.
    pub fallback_label: Option<String>,
    /// Which authenticators may satisfy the request.
    pub policy: AuthPolicy,
    /// Android: require an explicit confirmation tap after passive
    /// modalities such as face unlock.
    pub confirmation_required: bool,
}

impl AuthPrompt {
    /// Prompt with the given `title` and `reason`, the
    /// [`AuthPolicy::BiometricStrong`] policy and explicit confirmation.
    pub fn new(title: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            reason: reason.into(),
            subtitle: None,
            cancel_label: None,
            fallback_label: None,
            policy: AuthPolicy::default(),
            confirmation_required: true,
        }
    }

    #[must_use]
    pub fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    #[must_use]
    pub fn cancel_label(mut self, label: impl Into<String>) -> Self {
        self.cancel_label = Some(label.into());
        self
    }

    #[must_use]
    pub fn fallback_label(mut self, label: impl Into<String>) -> Self {
        self.fallback_label = Some(label.into());
        self
    }

    #[must_use]
    pub const fn policy(mut self, policy: AuthPolicy) -> Self {
        self.policy = policy;
        self
    }

    #[must_use]
    pub const fn confirmation_required(mut self, required: bool) -> Self {
        self.confirmation_required = required;
        self
    }
}

/// Factor the user verified with in a successful
/// [`Biometric::authenticate`].
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthMethod {
    Biometric,
    DeviceCredential,
    /// The platform does not report which factor was used — Windows
    /// Hello, Apple with [`AuthPolicy::BiometricOrDeviceCredential`],
    /// Android before API 30.
    Unspecified,
}

/// Domain-level errors surfaced by every [`Biometric`] method.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BiometricError {
    /// The user dismissed the prompt or pressed the cancel button.
    UserCancelled,
    /// The system dismissed the prompt — app moved to the background,
    /// another prompt took over, or the call was cancelled.
    SystemCancelled,
    /// The user pressed the fallback button (Apple
    /// [`AuthPrompt::fallback_label`]). Show your own password flow.
    UserFallback,
    /// The policy cannot be satisfied; carries the reason
    /// [`Biometric::availability`] would report.
    NotAvailable(BiometricStatus),
    /// Too many failed attempts; biometrics are temporarily locked.
    LockedOut,
    /// Biometrics are locked until the user authenticates with the
    /// device credential.
    LockedOutPermanent,
    /// The user failed to verify (Apple after three attempts, fprintd
    /// after its retry budget).
    AuthFailed,
    /// The [`AuthPrompt`] cannot be shown as configured, e.g. an empty
    /// `reason`.
    InvalidPrompt(String),
    /// Any other backend failure. Message is the raw platform error.
    Backend(String),
}

impl std::fmt::Display for BiometricError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserCancelled => f.write_str("biometric prompt cancelled by user"),
            Self::SystemCancelled => f.write_str("biometric prompt cancelled by the system"),
            Self::UserFallback => f.write_str("user chose the fallback authentication"),
            Self::NotAvailable(status) => {
                write!(f, "biometric authentication unavailable: {status:?}")
            }
            Self::LockedOut => f.write_str("biometrics temporarily locked out"),
            Self::LockedOutPermanent => {
                f.write_str("biometrics locked out until the device credential is entered")
            }
            Self::AuthFailed => f.write_str("biometric verification failed"),
            Self::InvalidPrompt(msg) => write!(f, "invalid biometric prompt: {msg}"),
            Self::Backend(msg) => write!(f, "biometric backend error: {msg}"),
        }
    }
}

impl std::error::Error for BiometricError {}

/// Recover the typed domain error from a failed [`BiometricClient`]
/// call. Transport failures (runtime shut down, unknown plugin, …) are
/// handed back unchanged as the `Err` value.
///
/// ```ignore
/// match client.authenticate(prompt).await {
///     Ok(method) => unlock(method),
///     Err(err) => match BiometricError::try_from(err) {
///         Ok(BiometricError::UserCancelled) => {}
///         Ok(domain) => show(domain),
///         Err(transport) => log(transport),
///     },
/// }
/// ```
impl TryFrom<IstmoError> for BiometricError {
    type Error = IstmoError;

    fn try_from(err: IstmoError) -> Result<Self, Self::Error> {
        match err {
            IstmoError::PluginError { bytes } => match codec::decode::<Self>(&bytes) {
                Ok((domain, _)) => Ok(domain),
                Err(_) => Err(IstmoError::PluginError { bytes }),
            },
            other => Err(other),
        }
    }
}

/// The plugin trait.
///
/// Native backends implement this to serve biometric requests; Rust
/// code calls it via the macro-generated [`BiometricClient`].
#[plugin(name = "istmo.biometric", crate = "::istmo_core")]
pub trait Biometric {
    /// Whether `policy` can be satisfied right now. Never shows UI.
    async fn availability(&self, policy: AuthPolicy) -> Result<Availability, BiometricError>;

    /// Present the system prompt and wait for the user.
    ///
    /// Dropping the returned future cancels the prompt where the
    /// platform allows it.
    async fn authenticate(
        &self,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<AuthMethod, BiometricError>;
}
