//! Runtime permission plugin.
//!
//! Wraps the platform's permission subsystem — Android's `checkSelfPermission`
//! / `requestPermissions` and iOS's per-framework authorisation APIs —
//! behind a single async surface.

use istmo_macros::plugin;

/// Wire identifier of the permissions plugin.
pub const PERMISSIONS_PLUGIN_ID: &str = "istmo.permissions";

// `PermissionStatus` + `PermissionOutcome` are generated from the
// canonical `Contract` builder in `istmo-plugins-schema` via
// `build.rs`. The include! lands the raw
// `#[::istmo_macros::message(bincode = "::bincode")] pub enum ...`
// declarations here; doc comments on individual variants live with the
// schema definition.
include!(concat!(env!("OUT_DIR"), "/permissions_types.rs"));

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
