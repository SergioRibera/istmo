//! Runtime permission requests.
//!
//! Query and request platform permissions (camera, microphone,
//! location, notifications, …) through a single async plugin surface.

use istmo_macros::plugin;

/// Wire identifier for the permissions plugin.
pub const PERMISSIONS_PLUGIN_ID: &str = "istmo.permissions";

include!(concat!(env!("OUT_DIR"), "/permissions_types.rs"));

#[plugin(name = "istmo.permissions", crate = "::istmo_core")]
pub trait Permissions {
    async fn check(&self, permission: String) -> PermissionStatus;

    async fn request(&self, permissions: Vec<String>) -> Vec<PermissionOutcome>;

    async fn should_show_rationale(&self, permission: String) -> bool;
}

