use core::fmt;
use std::io;

#[derive(Debug)]
pub enum AndroidRuntimeError {
    AlreadyStarted,

    NotStarted,

    /// `nativeStart` after `nativeShutdown`. The runtime lives in a
    /// process-wide `OnceLock`, so it cannot be brought back up.
    ShutDown,

    /// A JNI argument outside the range the Kotlin side is allowed to
    /// send (e.g. an unknown stream-end reason tag).
    InvalidArgument(String),

    /// The pump thread could not be spawned or attached to the JVM.
    Pump(io::Error),

    /// The `AssetManager` handed over by Kotlin could not be adopted.
    Assets(io::Error),

    Jni(jni::errors::Error),

    Core(istmo_core::IstmoError),
}

impl AndroidRuntimeError {
    /// `true` for routing misses that are expected races rather than
    /// caller bugs: a response landing after its call was cancelled, an
    /// event after its stream closed, a frame racing shutdown. These are
    /// logged instead of thrown so Kotlin callbacks that submit from a
    /// `catch` block cannot crash the app over them.
    pub(crate) const fn is_benign_race(&self) -> bool {
        matches!(
            self,
            Self::Core(
                istmo_core::IstmoError::UnknownCallId(_)
                    | istmo_core::IstmoError::UnknownStreamId(_)
                    | istmo_core::IstmoError::ChannelClosed
            )
        )
    }

    /// `true` when the failure came from a Java exception that is still
    /// pending on the calling thread; JNI exports leave it in place so
    /// Kotlin sees the original exception.
    pub(crate) const fn is_java_exception(&self) -> bool {
        matches!(self, Self::Jni(jni::errors::Error::JavaException))
    }
}

impl fmt::Display for AndroidRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyStarted => f.write_str("android runtime already started"),
            Self::NotStarted => f.write_str("android runtime not started"),
            Self::ShutDown => {
                f.write_str("android runtime was shut down and cannot be restarted in this process")
            }
            Self::InvalidArgument(what) => write!(f, "invalid argument: {what}"),
            Self::Pump(err) => write!(f, "pump thread failure: {err}"),
            Self::Assets(err) => write!(f, "asset manager failure: {err}"),
            Self::Jni(err) => write!(f, "jni failure: {err}"),
            Self::Core(err) => write!(f, "core runtime error: {err}"),
        }
    }
}

impl std::error::Error for AndroidRuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AlreadyStarted | Self::NotStarted | Self::ShutDown | Self::InvalidArgument(_) => {
                None
            }
            Self::Pump(err) | Self::Assets(err) => Some(err),
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

#[cfg(test)]
mod tests {
    use istmo_core::{CallId, IstmoError, StreamId};

    use super::*;

    #[test]
    fn routing_misses_are_benign_races() {
        assert!(AndroidRuntimeError::Core(IstmoError::UnknownCallId(CallId(7))).is_benign_race());
        assert!(
            AndroidRuntimeError::Core(IstmoError::UnknownStreamId(StreamId(7))).is_benign_race()
        );
        assert!(AndroidRuntimeError::Core(IstmoError::ChannelClosed).is_benign_race());
    }

    #[test]
    fn caller_bugs_are_not_benign() {
        assert!(!AndroidRuntimeError::NotStarted.is_benign_race());
        assert!(!AndroidRuntimeError::ShutDown.is_benign_race());
        assert!(!AndroidRuntimeError::Core(IstmoError::RuntimeNotStarted).is_benign_race());
        assert!(
            !AndroidRuntimeError::Core(IstmoError::ProtocolVersionMismatch {
                expected: 4,
                got: 3
            })
            .is_benign_race()
        );
        assert!(!AndroidRuntimeError::InvalidArgument("x".into()).is_benign_race());
    }

    #[test]
    fn only_pending_java_exceptions_are_left_in_place() {
        assert!(AndroidRuntimeError::Jni(jni::errors::Error::JavaException).is_java_exception());
        assert!(!AndroidRuntimeError::Jni(jni::errors::Error::NullPtr("x")).is_java_exception());
        assert!(!AndroidRuntimeError::NotStarted.is_java_exception());
    }
}
