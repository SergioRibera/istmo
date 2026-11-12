//! App lifecycle stream.
//!
//! Publishes coarse-grained lifecycle states (foreground, background,
//! terminated, …) through a latest-value slot so late subscribers see
//! the current state immediately on attach.

use istmo_macros::message;

use crate::early::{EarlyStream, LatestClient, LatestValue};

/// Plugin id of the lifecycle stream.
pub const LIFECYCLE_PLUGIN_ID: &str = "istmo.lifecycle";

/// Early-event channel name lifecycle updates are published on.
pub const LIFECYCLE_CHANNEL: &str = "istmo.lifecycle";

/// Lifecycle phase of the app's main surface, named after Android's
/// activity callbacks; iOS maps its scene phases onto the same set.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecycleState {
    /// Process and UI created.
    Created,
    /// Visible, not yet interactive.
    Started,
    /// In the foreground and interactive.
    Resumed,
    /// Losing focus.
    Paused,
    /// No longer visible.
    Stopped,
    /// UI torn down.
    Destroyed,
    /// The OS asked the app to free memory.
    LowMemory,
    /// Device configuration (orientation, locale, …) changed.
    ConfigurationChanged,
}

impl LatestValue for LifecycleState {
    const PLUGIN_ID: &'static str = LIFECYCLE_PLUGIN_ID;
    const CHANNEL: &'static str = LIFECYCLE_CHANNEL;
}

/// Reader for the lifecycle state the platform publishes.
pub type AppLifecycle = LatestClient<LifecycleState>;

/// Lifecycle states as they are published.
pub type LifecycleStream = EarlyStream<LifecycleState>;
