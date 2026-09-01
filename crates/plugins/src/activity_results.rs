//! Activity-result plugin.
//!
//! Wraps `Activity.startActivityForResult` on Android and
//! `UIViewController.present` with a completion handler on iOS behind a
//! single unary call. The plugin is stateless — callers submit an
//! [`IntentRequest`] and await the corresponding [`ActivityResult`].
//!
//! Domain-side "the intent could not be launched" (no matching activity,
//! entitlement missing) is separate from "the user cancelled" — the former
//! surfaces as [`ActivityLaunchError`], the latter as
//! [`ActivityOutcome::Cancelled`].

use istmo_macros::{message, plugin};

/// Wire identifier of the activity-results plugin.
pub const ACTIVITY_RESULTS_PLUGIN_ID: &str = "istmo.activity_results";

/// Typed value carried in intent extras.
///
/// The variant set is intentionally small: it covers the platform primitives
/// used by 95% of intents (text, numeric flags, boolean toggles, blobs).
/// Plugin authors who need richer structure should serialise it into
/// [`ExtraValue::Bytes`] with their own codec.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtraValue {
    Text(String),
    Int(i64),
    Bool(bool),
    Bytes(Vec<u8>),
}

/// Request describing an intent / activity to launch.
///
/// The shape mirrors Android's `Intent` semantics, which is a superset of
/// what iOS needs: iOS backends translate `action` + `uri` into the
/// appropriate `UIApplication.open` / `SFSafariViewController` /
/// `UIActivityViewController` invocation.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentRequest {
    /// Platform action string (e.g. `"android.intent.action.VIEW"`) or a
    /// well-known iOS role (`"share"`, `"open-url"`).
    pub action: String,
    /// URI carried by the intent (share URL, `mailto:`, `tel:` link). `None`
    /// for intents that only use extras.
    pub uri: Option<String>,
    /// Explicit component: `(package, class)` on Android, ignored on iOS.
    pub component: Option<(String, String)>,
    /// MIME type hint (`"image/png"`, `"text/plain"`).
    pub mime_type: Option<String>,
    /// Categories added to the intent (Android). Ignored on iOS.
    pub categories: Vec<String>,
    /// Intent extras. Order is preserved.
    pub extras: Vec<(String, ExtraValue)>,
}

/// Outcome flag returned by the platform.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityOutcome {
    /// The operation completed successfully (Android `RESULT_OK`, iOS
    /// completion handler with `completed = true`).
    Ok,
    /// The user cancelled (Android `RESULT_CANCELED`, iOS completion handler
    /// with `completed = false`).
    Cancelled,
    /// Platform returned a custom result code (Android only).
    Custom(i32),
}

/// Result payload delivered by the platform once the launched activity
/// terminates.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityResult {
    pub outcome: ActivityOutcome,
    /// Result URI (`data` field of Android's return `Intent`, iOS share URL,
    /// ...).
    pub data_uri: Option<String>,
    /// Extras attached to the result intent.
    pub extras: Vec<(String, ExtraValue)>,
}

/// Reasons an intent could not be launched at all.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityLaunchError {
    /// No installed application can handle the intent
    /// (`ActivityNotFoundException` on Android, `canOpenURL` false on iOS).
    NoActivityFound,
    /// The caller lacks the entitlement required to launch this intent
    /// (iOS restricted URL scheme, Android runtime permission missing).
    NotAuthorized,
    /// Free-form platform error message. Reserved for the long tail of
    /// exceptions the backend cannot map to a typed variant.
    Platform(String),
}

#[plugin(name = "istmo.activity_results", crate = "::istmo_core")]
pub trait ActivityResults {
    /// Launches `request` and awaits the terminal result.
    async fn launch(&self, request: IntentRequest) -> Result<ActivityResult, ActivityLaunchError>;
}
