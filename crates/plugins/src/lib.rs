//! Core plugins bundled with the istmo framework.
//!
//! Milestone M3 lands four capabilities that every mobile app needs and that
//! every downstream plugin will lean on. This commit adds the [`deeplinks`]
//! plugin alongside [`permissions`] and [`lifecycle`]; `activity_results`
//! follows.
//!
//! The plugin id namespace used on the wire is `istmo.<name>`; native
//! backends register handlers under those ids.

pub mod deeplinks;
pub mod lifecycle;
pub mod permissions;

pub use crate::deeplinks::{DEEPLINKS_CHANNEL, DeepLink, DeepLinkStream, DeepLinks};
pub use crate::lifecycle::{AppLifecycle, LIFECYCLE_CHANNEL, LifecycleState, LifecycleStream};
pub use crate::permissions::{
    PERMISSIONS_PLUGIN_ID, PermissionOutcome, PermissionStatus, Permissions,
};
