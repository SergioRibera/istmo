//! Public error type for the Android transport layer.

use core::fmt;

/// Errors surfaced from `istmo-android`.
#[derive(Debug)]
pub enum AndroidRuntimeError {
    /// `nativeStart` was called more than once.
    AlreadyStarted,
    /// `nativeSubmitFrame` / `nativeShutdown` fired before `nativeStart`.
    NotStarted,
    /// A JNI call (attach thread, allocate ref, look up method) failed.
    Jni(jni::errors::Error),
    /// The underlying core runtime returned an error.
    Core(istmo_core::IstmoError),
}

impl fmt::Display for AndroidRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyStarted => f.write_str("android runtime already started"),
            Self::NotStarted => f.write_str("android runtime not started"),
            Self::Jni(err) => write!(f, "jni failure: {err}"),
            Self::Core(err) => write!(f, "core runtime error: {err}"),
        }
    }
}

impl std::error::Error for AndroidRuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AlreadyStarted | Self::NotStarted => None,
            Self::Jni(err) => Some(err),
            Self::Core(err) => Some(err),
        }
    }
}

impl From<istmo_core::IstmoError> for AndroidRuntimeError {
    fn from(value: istmo_core::IstmoError) -> Self {
        Self::Core(value)
    }
}

impl From<jni::errors::Error> for AndroidRuntimeError {
    fn from(value: jni::errors::Error) -> Self {
        Self::Jni(value)
    }
}
