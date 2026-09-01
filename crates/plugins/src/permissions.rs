//! Runtime permission plugin.
//!
//! Wraps the platform's permission subsystem — Android's `checkSelfPermission`
//! / `requestPermissions` and iOS's per-framework authorisation APIs —
//! behind a single async surface.

use istmo_macros::{message, plugin};

/// Wire identifier of the permissions plugin.
pub const PERMISSIONS_PLUGIN_ID: &str = "istmo.permissions";

/// State of a single permission for the current process.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionStatus {
    /// The user has granted the permission.
    Granted,
    /// The user has denied the permission but the OS still lets us ask again.
    Denied,
    /// The user has denied the permission with a "don't ask again" flag, or
    /// policy blocks it entirely. Requesting again will not surface a prompt.
    PermanentlyDenied,
    /// The permission has never been requested, so the OS has no decision on
    /// record yet.
    NotDetermined,
    /// The permission does not exist on the current platform / OS version.
    NotSupported,
}

/// Outcome for a single permission emitted by [`Permissions::request`].
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionOutcome {
    /// The platform-specific permission identifier (e.g.
    /// `"android.permission.CAMERA"`).
    pub permission: String,
    /// The status observed after the request completed.
    pub status: PermissionStatus,
}

#[plugin(name = "istmo.permissions", crate = "::istmo_core")]
pub trait Permissions {
    /// Returns the current status of `permission` without prompting the user.
    async fn check(&self, permission: String) -> PermissionStatus;

    /// Requests the given permissions. The returned vector contains one
    /// [`PermissionOutcome`] per input, preserving the input order.
    async fn request(&self, permissions: Vec<String>) -> Vec<PermissionOutcome>;

    /// Returns `true` when the platform recommends showing a rationale UI
    /// before prompting again — i.e. the user has denied at least once but
    /// the permission is not permanently denied. Always `false` on iOS.
    async fn should_show_rationale(&self, permission: String) -> bool;
}
