use core::fmt;

#[derive(Debug)]
pub enum IosRuntimeError {
    AlreadyStarted,

    NotStarted,

    InvalidUtf8,

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
