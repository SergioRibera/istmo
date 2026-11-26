//! `CaptureError`: every failure names its backend and its step.

use std::fmt;

use istmo_core::IstmoError;
use istmo_macros::message;

use super::caps::{Availability, OsVersion};
use super::source::{BackendId, SourceId, SourceKind};

/// One requirement a backend could not meet.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Requirement {
    /// The source kind (window / application) is not capturable here.
    // Not `SourceKind(SourceKind)`: a Kotlin nested class named like its
    // payload type shadows it in the generated codecs.
    Kind(SourceKind),
    /// A window or application by id on a picker-only backend.
    WindowById,
    /// A picker was needed and `allow_picker` is false.
    Picker,
    /// `Focused` / `UnderCursor` on a backend that cannot resolve them.
    SourceSelector,
    /// Several monitors on a backend or platform with one display.
    MultiMonitor,
    /// A multi-monitor or spanning-region stream (not implemented yet).
    CompositeStreaming,
    /// Preview and frames without the fan-out method.
    PreviewAndFrames,
    /// No listed format tier can be delivered.
    Formats,
    /// `Scaled` / `Fit` without scaling or a tier allowing the copy.
    Scaling,
    /// `cursor.strength = Require`.
    Cursor,
    /// `damage = Require`.
    Damage,
    /// `RequireHdr`.
    Hdr,
    /// `CopyToCpu` without a readback for the negotiated memory.
    ExhaustionReadback,
    /// `GpuPreference::Same` without a tier allowing `CrossDevice`.
    Gpu,
    /// `Borderless::Require`.
    Borderless,
    /// `OrientationHandling::Upright` for memory the plugin cannot turn.
    Upright,
    /// Audio on a backend without it.
    Audio,
    /// A region outside every source.
    Region,
    /// The operation (e.g. preview) is not offered on this platform.
    Operation,
    /// A composite larger than the plugin allocates (see
    /// `composite::MAX_COMPOSITE_PIXELS`), or mostly gaps between
    /// far-apart monitors.
    CompositeSize,
}

/// What a backend could not do.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Unmet {
    pub backend: Option<BackendId>,
    pub requirements: Vec<Requirement>,
}

/// Why one backend of an `Ordered` / `Auto` walk was passed over.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AttemptFailure {
    Unavailable(Availability),
    Unsatisfiable(Vec<Requirement>),
    /// It was tried and failed (the error, rendered).
    Failed(String),
}

/// One backend of a failed walk.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BackendAttempt {
    pub backend: BackendId,
    pub failure: AttemptFailure,
}

/// A backend that cannot run, and why.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BackendReason {
    pub backend: BackendId,
    pub availability: Availability,
}

/// A protocol-level failure: backend, step, message.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BackendDetail {
    pub backend: Option<BackendId>,
    pub step: String,
    pub message: String,
}

impl BackendDetail {
    #[must_use]
    pub fn new(backend: BackendId, step: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            backend: Some(backend),
            step: step.into(),
            message: message.into(),
        }
    }
}

/// An OS call that failed: backend, operation, error code.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OsFailure {
    pub backend: BackendId,
    pub operation: String,
    pub code: i64,
}

/// Every error of the screen-capture plugin.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CaptureError {
    /// This build has no backend for this platform.
    NoBackendCompiled,
    /// The named backend is not compiled in (its cargo feature is off).
    BackendNotCompiled(BackendId),
    BackendUnavailable(BackendReason),
    /// A requirement the chosen backend (or every candidate) cannot meet.
    Unsatisfiable(Unmet),
    /// Every candidate was passed over.
    NoBackend(Vec<BackendAttempt>),
    SourceNotFound(SourceId),
    SourceLost,
    /// Consent is needed and `allow_picker` is false.
    ConsentRequired,
    ConsentDenied,
    ConsentRevoked,
    AccessRequiresRestart,
    OsTooOld(OsVersion),
    /// The method is not open to callers outside this process.
    OriginRefused,
    PolicyDenied(String),
    /// The frame sink is closed or belongs to another runtime.
    SinkUnreachable,
    SessionNotFound,
    GrantExpired,
    /// The session lost its format and no lower tier was listed.
    FormatLost,
    Protocol(BackendDetail),
    Os(OsFailure),
}

impl CaptureError {
    /// `Unsatisfiable` for one backend and one requirement.
    #[must_use]
    pub fn unsatisfiable(backend: Option<BackendId>, requirement: Requirement) -> Self {
        Self::Unsatisfiable(Unmet {
            backend,
            requirements: vec![requirement],
        })
    }

    /// `Protocol` for a backend step.
    #[must_use]
    pub fn protocol(
        backend: BackendId,
        step: impl Into<String>,
        message: impl fmt::Display,
    ) -> Self {
        Self::Protocol(BackendDetail::new(backend, step, message.to_string()))
    }

    /// `Os` for a failed OS call.
    #[must_use]
    pub fn os(backend: BackendId, operation: impl Into<String>, code: i64) -> Self {
        Self::Os(OsFailure {
            backend,
            operation: operation.into(),
            code,
        })
    }

    /// Recover the typed error from a failed client call.
    ///
    /// # Errors
    /// Hands `err` back when it is a transport failure or does not decode
    /// as a [`CaptureError`].
    pub fn try_from_istmo(err: IstmoError) -> Result<Self, IstmoError> {
        match err {
            IstmoError::PluginError { bytes } => match istmo_core::codec::decode::<Self>(&bytes) {
                Ok((domain, _)) => Ok(domain),
                Err(_) => Err(IstmoError::PluginError { bytes }),
            },
            other => Err(other),
        }
    }
}

/// Lossy: transport failures become [`CaptureError::Protocol`] with no
/// backend. Use [`CaptureError::try_from_istmo`] to tell them apart.
impl From<IstmoError> for CaptureError {
    fn from(err: IstmoError) -> Self {
        Self::try_from_istmo(err).unwrap_or_else(|err| {
            Self::Protocol(BackendDetail {
                backend: None,
                step: "transport".to_owned(),
                message: err.to_string(),
            })
        })
    }
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kind(kind) => write!(f, "{kind:?} sources"),
            Self::WindowById => f.write_str("a window by id"),
            Self::Picker => f.write_str("a picker (allow_picker is false)"),
            Self::SourceSelector => f.write_str("the focused / under-cursor selector"),
            Self::MultiMonitor => f.write_str("several monitors"),
            Self::CompositeStreaming => f.write_str("a multi-monitor stream"),
            Self::PreviewAndFrames => f.write_str("preview and frames together"),
            Self::Formats => f.write_str("any listed format tier"),
            Self::Scaling => f.write_str("scaling"),
            Self::Cursor => f.write_str("the required cursor mode"),
            Self::Damage => f.write_str("damage rectangles"),
            Self::Hdr => f.write_str("HDR"),
            Self::ExhaustionReadback => f.write_str("CPU readback on exhaustion"),
            Self::Gpu => f.write_str("the requested GPU"),
            Self::Borderless => f.write_str("borderless capture"),
            Self::Upright => f.write_str("upright buffers"),
            Self::Audio => f.write_str("audio"),
            Self::Region => f.write_str("the region"),
            Self::Operation => f.write_str("this operation"),
            Self::CompositeSize => f.write_str("a composite of this size"),
        }
    }
}

fn backend_prefix(f: &mut fmt::Formatter<'_>, backend: Option<BackendId>) -> fmt::Result {
    backend.map_or(Ok(()), |backend| write!(f, "{backend}: "))
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoBackendCompiled => {
                f.write_str("no screen-capture backend is compiled for this platform")
            }
            Self::BackendNotCompiled(backend) => {
                write!(
                    f,
                    "{backend}: not compiled (enable the `{backend}` feature)"
                )
            }
            Self::BackendUnavailable(reason) => {
                write!(
                    f,
                    "{}: unavailable ({:?})",
                    reason.backend, reason.availability
                )
            }
            Self::Unsatisfiable(unmet) => {
                backend_prefix(f, unmet.backend)?;
                f.write_str("cannot provide ")?;
                for (i, requirement) in unmet.requirements.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{requirement}")?;
                }
                Ok(())
            }
            Self::NoBackend(attempts) => {
                f.write_str("no backend could serve the call")?;
                for attempt in attempts {
                    write!(f, "; {}: {:?}", attempt.backend, attempt.failure)?;
                }
                Ok(())
            }
            Self::SourceNotFound(id) => {
                write!(f, "{}: source `{}` not found", id.backend, id.value)
            }
            Self::SourceLost => f.write_str("the captured source went away"),
            Self::ConsentRequired => f.write_str("consent is required and allow_picker is false"),
            Self::ConsentDenied => f.write_str("consent was denied"),
            Self::ConsentRevoked => f.write_str("consent was revoked"),
            Self::AccessRequiresRestart => {
                f.write_str("the grant applies only after the app restarts")
            }
            Self::OsTooOld(v) => {
                write!(f, "OS too old (needs {}.{}.{})", v.major, v.minor, v.patch)
            }
            Self::OriginRefused => f.write_str("this method only answers callers in this process"),
            Self::PolicyDenied(why) => write!(f, "denied by the capture policy: {why}"),
            Self::SinkUnreachable => f.write_str("the frame sink is closed or not in this runtime"),
            Self::SessionNotFound => f.write_str("no such capture session"),
            Self::GrantExpired => f.write_str("the projection grant expired"),
            Self::FormatLost => {
                f.write_str("the session lost its format and no lower tier was listed")
            }
            Self::Protocol(detail) => {
                backend_prefix(f, detail.backend)?;
                write!(f, "{}: {}", detail.step, detail.message)
            }
            Self::Os(os) => write!(
                f,
                "{}: {} failed (code {})",
                os.backend, os.operation, os.code
            ),
        }
    }
}

impl std::error::Error for CaptureError {}
