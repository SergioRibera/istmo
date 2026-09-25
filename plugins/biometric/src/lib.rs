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
//! Protect real secrets with the vault below instead.
//!
//! # Biometric-bound secrets
//!
//! [`Biometric::store_secret`] / [`Biometric::read_secret`] keep small
//! secrets (tokens, keys) encrypted under key material the OS only
//! releases after a successful verification:
//!
//! | Platform | Storage                                                                 |
//! | -------- | ----------------------------------------------------------------------- |
//! | Android  | AES-GCM Keystore key bound to `BiometricPrompt.CryptoObject`            |
//! | iOS      | Keychain item with `SecAccessControl` (`.biometryCurrentSet` / `.userPresence`) |
//! | macOS    | Same as iOS, in the data-protection keychain (signed apps only)          |
//! | Windows  | AES-GCM key derived from a Windows Hello (`KeyCredentialManager`) signature |
//! | Linux    | Unsupported — fprintd exposes no key material                           |
//!
//! Secrets are addressed by a [`SecretAlias`]. With a biometric-only
//! policy, enrolling a new fingerprint or face destroys the secret's
//! key: Android reports [`BiometricError::KeyInvalidated`], Apple
//! platforms stop finding the item ([`BiometricError::SecretNotFound`]).
//! Windows reports [`BiometricError::KeyInvalidated`] when the Hello key
//! itself is gone (PIN reset, Hello removed).
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
    /// Fingerprint reader (Touch ID, capacitive or under-display sensors).
    Fingerprint,
    /// Face recognition (Face ID, Android face unlock).
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

/// Longest accepted [`SecretAlias`], in bytes.
pub const MAX_SECRET_ALIAS_LEN: usize = 64;

/// Name of a biometric-bound secret: 1 to [`MAX_SECRET_ALIAS_LEN`] ASCII
/// letters, digits, `.`, `_` or `-`, so every backend can use it as a
/// file name, preference key or keychain account verbatim.
///
/// The wire carries plain strings; every backend re-validates them and
/// rejects bad ones with [`BiometricError::InvalidAlias`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SecretAlias(String);

impl SecretAlias {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for SecretAlias {
    type Error = BiometricError;

    fn try_from(alias: &str) -> Result<Self, Self::Error> {
        let valid_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
        if alias.is_empty() || alias.len() > MAX_SECRET_ALIAS_LEN || !alias.chars().all(valid_char)
        {
            return Err(BiometricError::InvalidAlias(alias.to_owned()));
        }
        Ok(Self(alias.to_owned()))
    }
}

impl TryFrom<String> for SecretAlias {
    type Error = BiometricError;

    fn try_from(alias: String) -> Result<Self, Self::Error> {
        Self::try_from(alias.as_str())
    }
}

impl From<SecretAlias> for String {
    fn from(alias: SecretAlias) -> Self {
        alias.0
    }
}

impl std::fmt::Display for SecretAlias {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Factor the user verified with in a successful
/// [`Biometric::authenticate`].
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthMethod {
    /// A biometric sensor verified the user.
    Biometric,
    /// The user entered the device PIN / pattern / password.
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
    /// The alias is not a valid [`SecretAlias`].
    InvalidAlias(String),
    /// No secret is stored under the alias.
    SecretNotFound,
    /// The key sealing the secret is gone — biometric enrollment changed
    /// (Android) or the Windows Hello key was reset. The secret is lost;
    /// store it again.
    KeyInvalidated,
    /// The operation is not available on this platform (biometric-bound
    /// secrets on Linux, the macOS keychain in unsigned binaries).
    UnsupportedOperation(String),
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
            Self::InvalidAlias(alias) => write!(f, "invalid secret alias `{alias}`"),
            Self::SecretNotFound => f.write_str("no secret stored under this alias"),
            Self::KeyInvalidated => {
                f.write_str("secret invalidated by a biometric enrollment change")
            }
            Self::UnsupportedOperation(msg) => write!(f, "unsupported biometric operation: {msg}"),
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

    /// Encrypt `secret` under key material bound to `prompt.policy` and
    /// store it as `alias`, replacing any previous value.
    ///
    /// Android and Windows verify the user before encrypting, so they
    /// show `prompt`; Apple platforms store without UI. Biometric-only
    /// policies are treated as [`AuthPolicy::BiometricStrong`] — the only
    /// class Android allows to unlock keys.
    ///
    /// # macOS
    ///
    /// Every vault method needs an app signed with a Team ID and the
    /// `keychain-access-groups` entitlement (granted by an embedded
    /// provisioning profile). Unsigned binaries — including `cargo run`
    /// and ad-hoc signed builds — get
    /// [`BiometricError::UnsupportedOperation`].
    async fn store_secret(
        &self,
        alias: String,
        secret: Vec<u8>,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<(), BiometricError>;

    /// Verify the user with `prompt` and decrypt the secret stored as
    /// `alias`. The policy it was stored with applies; `prompt.policy`
    /// only shapes the dialog.
    async fn read_secret(
        &self,
        alias: String,
        prompt: AuthPrompt,
        cancel: CancelToken,
    ) -> Result<Vec<u8>, BiometricError>;

    /// Delete the secret stored as `alias` and its key. Succeeds when
    /// nothing is stored. Never shows UI.
    async fn delete_secret(&self, alias: String) -> Result<(), BiometricError>;

    /// Whether a secret is stored as `alias`. Never shows UI.
    async fn has_secret(&self, alias: String) -> Result<bool, BiometricError>;
}
