//! What a desktop backend implements, and the plans the host hands it.
//!
//! Every method may block (Wayland round trips, D-Bus, X11): the host
//! only calls them from its own threads (`offload`), never on the
//! caller's executor.

use istmo_core::NativeBuffer;

use crate::policy::CapturePolicy;
use crate::types::{
    AccessScope, AccessState, AudioCaps, AudioConfig, AudioNegotiated, Availability, BackendCaps,
    BackendId, CaptureError, ConsentConfig, CursorRequest, Exhaustion, FormatRequest, FrameMeta,
    FrameRate, GpuPreference, HdrPolicy, LinuxOptions, NegotiatedStream, OrientationHandling,
    OutputSize, PickerOptions, Rect, Requirement, SessionUpdate, Source, SourceId, SourceList,
    SourceSelector, Strength, WindowsOptions,
};

use super::offload::Caller;
use super::output::{AudioOut, FrameOut};
use super::watch::SourceFeed;

/// What to capture, resolved against the backend's listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// Concrete sources (one, or several for a composite), plus the
    /// global logical region to keep when the caller asked for one.
    Sources {
        sources: Vec<Source>,
        region: Option<Rect>,
    },
    /// The platform picker decides (portal and other picker-only
    /// backends).
    Picker(PickerOptions),
}

impl Target {
    /// Number of sources, 0 for a picker.
    pub(crate) const fn len(&self) -> usize {
        match self {
            Self::Sources { sources, .. } => sources.len(),
            Self::Picker(_) => 0,
        }
    }
}

/// A screenshot as the backend sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScreenshotPlan {
    pub(crate) target: Target,
    /// Never empty: the host puts a CPU tier first when the caller left
    /// it empty.
    pub(crate) formats: FormatRequest,
    pub(crate) cursor: CursorRequest,
    pub(crate) hdr: HdrPolicy,
    pub(crate) consent: ConsentConfig,
    pub(crate) linux: LinuxOptions,
    /// Whether the host call is still awaited: a consent dialog nobody
    /// waits for is closed.
    pub(crate) caller: Caller,
}

/// One captured frame.
#[derive(Debug)]
pub(crate) struct Captured {
    pub(crate) meta: FrameMeta,
    pub(crate) buffer: NativeBuffer,
    /// The source as captured (geometry, scale, orientation).
    pub(crate) source: Source,
}

/// A finished screenshot: one frame per source.
#[derive(Debug)]
pub(crate) struct Shot {
    pub(crate) frames: Vec<Captured>,
    pub(crate) negotiated: NegotiatedStream,
}

/// A live session as the backend sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamPlan {
    pub(crate) source: Source,
    pub(crate) formats: FormatRequest,
    pub(crate) fps: FrameRate,
    pub(crate) size: OutputSize,
    pub(crate) cursor: CursorRequest,
    pub(crate) damage: Strength,
    pub(crate) hdr: HdrPolicy,
    /// Already clamped to the backend's `PoolCaps`.
    pub(crate) pool_depth: u8,
    pub(crate) exhaustion: Exhaustion,
    pub(crate) gpu: GpuPreference,
    pub(crate) orientation: OrientationHandling,
    pub(crate) live_buffers: bool,
    /// Windows / applications to leave out, where the backend can.
    pub(crate) exclude: Vec<SourceId>,
    /// Leave this process's own windows and audio out, where the
    /// backend can.
    pub(crate) exclude_own_process: bool,
    pub(crate) consent: ConsentConfig,
    pub(crate) linux: LinuxOptions,
    pub(crate) windows: WindowsOptions,
    /// `CapturePolicy::allow_borderless`: whether a borderless request may
    /// prompt for the grant.
    pub(crate) allow_borderless: bool,
    /// For picker-only backends: what the picker offers.
    pub(crate) picker: Option<PickerOptions>,
    /// Whether the host call is still awaited.
    pub(crate) caller: Caller,
}

/// A running backend session. Dropping it stops the capture and
/// releases everything the backend holds; frames already delivered stay
/// valid until their consumers drop them.
pub(crate) trait BackendSession: Send {
    /// Changes only the backend can make (cursor, formats). Frame rate,
    /// pause / resume and in-meta region crops are handled by the host.
    ///
    /// # Errors
    /// `Unsatisfiable` when the backend cannot make the change; the
    /// session then stays as it was.
    fn update(&mut self, _update: &SessionUpdate) -> Result<NegotiatedStream, CaptureError> {
        Err(CaptureError::unsatisfiable(None, Requirement::Operation))
    }
}

/// A desktop capture backend.
pub(crate) trait Backend: Send + Sync + 'static {
    fn id(&self) -> BackendId;

    /// Whether it can run in this session. May connect to the display.
    fn availability(&self) -> Availability;

    /// Whether `Auto` may pick it. A backend joins `Auto` only once its
    /// checklist (open, frames, close without leaks, exhaustion, sink
    /// drop, pixel checks) passed on a real session; until then it is
    /// reachable through `Fixed`, `Ordered` and the environment
    /// override only.
    fn joins_auto(&self) -> bool {
        true
    }

    /// Whether a `SourceSelector::Picker` shows a system picker here
    /// although the backend also lists sources (macOS 14+
    /// `SCContentSharingPicker`). Picker-only backends say so through
    /// their `SourceListing` instead.
    fn offers_picker(&self) -> bool {
        false
    }

    /// The standing grant for `scope`, without prompting; `None` when
    /// the backend has none for it.
    fn access(&self, _scope: AccessScope) -> Option<AccessState> {
        None
    }

    /// Prompt for the standing grant for `scope` where `policy` lets the
    /// app ask for it; `None` when the backend has none for it.
    fn request_access(
        &self,
        _scope: AccessScope,
        _policy: &CapturePolicy,
    ) -> Option<Result<AccessState, CaptureError>> {
        None
    }

    /// The capability report, probed where possible, each section with
    /// its provenance.
    ///
    /// # Errors
    /// Probing failed (the backend is then reported as not ready).
    fn caps(&self) -> Result<BackendCaps, CaptureError>;

    /// Sources capturable without a picker.
    ///
    /// # Errors
    /// Enumeration failed.
    fn sources(&self) -> Result<SourceList, CaptureError>;

    /// Publish the sources into `feed` (what [`sources`](Self::sources)
    /// lists), then again on every change, until the feed's subscriber
    /// leaves. Blocks: the host runs it on a thread of its own; waits
    /// look at [`SourceFeed::is_closed`] at least every
    /// [`WATCH_POLL`](super::watch::WATCH_POLL).
    ///
    /// # Errors
    /// `Unsatisfiable(Operation)` by default; anything that ended the
    /// watch (the compositor or server went away).
    fn watch(&self, _feed: &mut SourceFeed) -> Result<(), CaptureError> {
        Err(CaptureError::unsatisfiable(
            Some(self.id()),
            Requirement::Operation,
        ))
    }

    /// Resolve `Focused` / `UnderCursor`.
    ///
    /// # Errors
    /// `Unsatisfiable(SourceSelector)` by default.
    fn resolve_special(&self, _selector: &SourceSelector) -> Result<Source, CaptureError> {
        Err(CaptureError::unsatisfiable(
            Some(self.id()),
            Requirement::SourceSelector,
        ))
    }

    /// One frame per target source, CPU tiers first when listed first.
    ///
    /// # Errors
    /// Anything that stopped the capture, naming the step.
    fn screenshot(&self, plan: &ScreenshotPlan) -> Result<Shot, CaptureError>;

    /// Start a live session delivering into `out`. Returns once the
    /// stream is negotiated (the first frame may still be on its way).
    ///
    /// # Errors
    /// Anything that stopped the negotiation, naming the step.
    fn open(
        &self,
        plan: StreamPlan,
        out: FrameOut,
    ) -> Result<(Box<dyn BackendSession>, NegotiatedStream), CaptureError>;
}

/// A desktop audio backend (independent of the video backend).
pub(crate) trait AudioBackend: Send + Sync + 'static {
    /// Whether it can run in this session. May connect to the daemon.
    fn availability(&self) -> Availability;

    fn caps(&self) -> AudioCaps;

    /// Start capturing audio into `out`.
    ///
    /// # Errors
    /// Anything that stopped the negotiation.
    fn open(
        &self,
        config: &AudioConfig,
        out: AudioOut,
    ) -> Result<(Box<dyn BackendSession>, AudioNegotiated), CaptureError>;
}
