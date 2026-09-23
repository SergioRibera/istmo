//! Launch an activity / view controller and await its result.
//!
//! The Rust side drives the round-trip; the platform side implements
//! it with `ActivityResultLauncher` on Android and equivalent APIs on
//! Apple platforms.

use istmo_macros::{message, plugin};

/// Wire identifier for the activity-results plugin.
pub const ACTIVITY_RESULTS_PLUGIN_ID: &str = "istmo.activity_results";

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtraValue {
    Text(String),
    Int(i64),
    Bool(bool),
    Bytes(Vec<u8>),
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentRequest {
    pub action: String,
    pub uri: Option<String>,
    pub component: Option<(String, String)>,
    pub mime_type: Option<String>,
    pub categories: Vec<String>,
    pub extras: Vec<(String, ExtraValue)>,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityOutcome {
    Ok,
    Cancelled,
    Custom(i32),
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityResult {
    pub outcome: ActivityOutcome,
    pub data_uri: Option<String>,
    pub extras: Vec<(String, ExtraValue)>,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityLaunchError {
    NoActivityFound,
    NotAuthorized,
    Platform(String),
}

impl core::fmt::Display for ActivityLaunchError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoActivityFound => f.write_str("no activity found for intent"),
            Self::NotAuthorized => f.write_str("activity launch not authorized"),
            Self::Platform(msg) => write!(f, "activity launch platform error: {msg}"),
        }
    }
}

impl std::error::Error for ActivityLaunchError {}

#[plugin(name = "istmo.activity_results", crate = "::istmo_core")]
pub trait ActivityResults {
    async fn launch(&self, request: IntentRequest) -> Result<ActivityResult, ActivityLaunchError>;
}
