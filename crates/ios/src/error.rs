use core::fmt;
use std::io;

#[derive(Debug)]
pub enum IosRuntimeError {
    AlreadyStarted,

    NotStarted,

    InvalidUtf8,

    /// A `(ptr, len)` pair with a null `ptr` and a non-zero `len`, or a
    /// `len` above `isize::MAX`.
    InvalidBuffer {
        len: usize,
    },

    /// `istmo_ios_start` received a callback table with this field null.
    NullCallback(&'static str),

    SpawnPump(io::Error),

    Core(istmo_core::IstmoError),
}

impl fmt::Display for IosRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyStarted => f.write_str("ios runtime already started"),
            Self::NotStarted => f.write_str("ios runtime not started"),
            Self::InvalidUtf8 => f.write_str("swift-supplied string was not valid utf-8"),
            Self::InvalidBuffer { len } => {
                write!(
                    f,
                    "swift-supplied buffer of {len} bytes is null or too large"
                )
            }
            Self::NullCallback(field) => write!(f, "callback table field `{field}` is null"),
            Self::SpawnPump(err) => write!(f, "failed to spawn the ios pump thread: {err}"),
            Self::Core(err) => write!(f, "core runtime error: {err}"),
        }
    }
}

impl std::error::Error for IosRuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AlreadyStarted
            | Self::NotStarted
            | Self::InvalidUtf8
            | Self::InvalidBuffer { .. }
            | Self::NullCallback(_) => None,
            Self::SpawnPump(err) => Some(err),
            Self::Core(err) => Some(err),
        }
    }
}

impl From<istmo_core::IstmoError> for IosRuntimeError {
    fn from(value: istmo_core::IstmoError) -> Self {
        Self::Core(value)
    }
}
