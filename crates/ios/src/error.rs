//! Public error type for the iOS transport layer.

use core::fmt;

/// Errors surfaced from `istmo-ios`.
#[derive(Debug)]
pub enum IosRuntimeError {
    /// `istmo_ios_start` was called more than once.
    AlreadyStarted,
    /// An inbound submission fired before `istmo_ios_start`.
    NotStarted,
    /// A UTF-8 buffer supplied by the Swift side was not valid UTF-8.
    InvalidUtf8,
    /// The underlying core runtime returned an error.
    Core(istmo_core::IstmoError),
}

impl fmt::Display for IosRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyStarted => f.write_str("ios runtime already started"),
            Self::NotStarted => f.write_str("ios runtime not started"),
            Self::InvalidUtf8 => f.write_str("swift-supplied string was not valid utf-8"),
            Self::Core(err) => write!(f, "core runtime error: {err}"),
        }
    }
}

impl std::error::Error for IosRuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AlreadyStarted | Self::NotStarted | Self::InvalidUtf8 => None,
            Self::Core(err) => Some(err),
        }
    }
}

impl From<istmo_core::IstmoError> for IosRuntimeError {
    fn from(value: istmo_core::IstmoError) -> Self {
        Self::Core(value)
    }
}
