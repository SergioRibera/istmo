//! Core plugins bundled with the istmo framework.
//!
//! Milestone M3 lands four capabilities that every mobile app needs and that
//! every downstream plugin will lean on. This first commit scaffolds the
//! crate and ships the [`permissions`] plugin; subsequent commits add
//! `lifecycle`, `deeplinks` and `activity_results`.
//!
//! The plugin id namespace used on the wire is `istmo.<name>`; native
//! backends register handlers under those ids.

pub mod permissions;

pub use crate::permissions::{
    PERMISSIONS_PLUGIN_ID, PermissionOutcome, PermissionStatus, Permissions,
};
