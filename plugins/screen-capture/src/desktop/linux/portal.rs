//! xdg-desktop-portal ScreenCast + PipeWire.
//!
//! `CreateSession` → `SelectSources` (types, multiple, `cursor_mode` on
//! v ≥ 2, `persist_mode` + `restore_token` on v ≥ 4) → `Start` (the
//! dialog) → one PipeWire stream per chosen source, on the portal's
//! private remote (`OpenPipeWireRemote`) or the session daemon
//! (`LinuxOptions::pipewire`). The token `Start` returns goes back to the
//! app in `NegotiatedStream::restore_token`; the plugin never stores it.
//!
//! Frames leave without a copy of the plugin's: DMA-BUF (modifiers from
//! the compositor's dmabuf feedback, explicit sync waited through DRM
//! syncobjs) or `MemFd` shared memory (`CopyStage::ShmTransport`, the
//! producer's copy), held dequeued while the consumer holds the frame
//! (see [`stream`]).

mod pods;
mod raw;
mod stream;

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use ashpd::desktop::screencast::{
    CursorMode as PortalCursor, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions,
    SourceType, StartCastOptions, Stream as PortalStream,
};
use ashpd::desktop::{CreateSessionOptions, PersistMode as PortalPersist, ResponseError, Session};

use crate::select;
use crate::types::{
    AudioCaps, Availability, BackendCaps, BackendId, BorderState, BufferMemory, CaptureError,
    Colour, ColourCaps, CompositeCaps, CompositeSupport, ConsentConfig, ConsentModel, CopyStage,
    CursorMode, CursorRequest, DamageSupport, Exhaustion, FormatCaps, FrameRate, GpuIdentity,
    IndicatorState, NegotiatedStream, OrientationSupport, PersistMode, Persistence, PickerOptions,
    PipeWireConnection, PoolCaps, ProtectedContent, Provenance, RESTORE_TOKEN_VERSION, Rect,
    RegionCrop, Requirement, RestoreToken, Scaling, Source, SourceId, SourceKind, SourceList,
    SourceListing, SyncKind, WindowCaptureCaps,
};

use self::pods::Imports;
use self::stream::{Live, Negotiated, Part, Setup};
use super::super::backend::{
    Backend, BackendSession, Captured, ScreenshotPlan, Shot, StreamPlan, Target,
};
use super::super::offload::Caller;
use super::super::output::FrameOut;
use super::pipewire::{LoopThread, Remote};

const ID: BackendId = BackendId::LinuxPortal;

pub(crate) fn backend() -> Arc<dyn Backend> {
    Arc::new(Portal)
}

fn portal_error(step: &str, err: &ashpd::Error) -> CaptureError {
    match err {
        ashpd::Error::Response(ResponseError::Cancelled) => CaptureError::ConsentDenied,
        other => CaptureError::protocol(ID, step, other),
    }
}

/// What the portal says about itself, without a dialog.
#[derive(Debug, Clone, Copy)]
struct Probe {
    version: u32,
    sources: SourceType,
    cursors: PortalCursor,
}

fn probe() -> Result<Probe, CaptureError> {
    pollster::block_on(async {
        let proxy = Screencast::new()
            .await
            .map_err(|e| portal_error("connect to the ScreenCast portal", &e))?;
        let version = proxy.version();
        let sources = proxy
            .available_source_types()
            .await
            .map_err(|e| portal_error("AvailableSourceTypes", &e))?;
        let cursors = if version >= 2 {
            proxy
                .available_cursor_modes()
                .await
                .map_err(|e| portal_error("AvailableCursorModes", &e))?
        } else {
            PortalCursor::empty()
        };
        Ok(Probe {
            version,
            sources,
            cursors,
        })
    })
}

impl Probe {
    fn source_kinds(self) -> Vec<SourceKind> {
        let mut kinds = Vec::new();
        if self.sources.contains(SourceType::MONITOR) {
            kinds.push(SourceKind::Monitor);
        }
        if self.sources.contains(SourceType::WINDOW) {
            kinds.push(SourceKind::Window);
        }
        kinds
    }

    /// Cursor modes this backend delivers: the portal's (metadata through
    /// the PipeWire cursor meta). Before v2 the portal takes no cursor
    /// mode and embeds the cursor.
    fn cursor_modes(self) -> Vec<CursorMode> {
        if self.version < 2 {
            return vec![CursorMode::Embedded];
        }
        let mut modes = Vec::new();
        if self.cursors.contains(PortalCursor::HIDDEN) {
            modes.push(CursorMode::Hidden);
        }
        if self.cursors.contains(PortalCursor::EMBEDDED) {
            modes.push(CursorMode::Embedded);
        }
        if self.cursors.contains(PortalCursor::METADATA) {
            modes.push(CursorMode::Metadata);
        }
        modes
    }

    const fn persists(self) -> bool {
        self.version >= 4
    }
}

/// The token of `consent` the portal can use: from this backend and this
/// token version, on a portal that persists. Anything else is ignored
/// (the dialog shows).
fn usable_token(consent: &ConsentConfig, persists: bool) -> Option<&str> {
    consent
        .restore_token
        .as_ref()
        .filter(|t| persists && t.backend == ID && t.plugin_version == RESTORE_TOKEN_VERSION)
        .map(|t| t.value.as_str())
}

const fn persist_mode(mode: PersistMode) -> PortalPersist {
    match mode {
        PersistMode::DoNot => PortalPersist::DoNot,
        PersistMode::WhileRunning => PortalPersist::Application,
        PersistMode::UntilRevoked => PortalPersist::ExplicitlyRevoked,
    }
}

fn source_types(options: &PickerOptions, probe: Probe) -> SourceType {
    let wanted = if options.kinds.is_empty() {
        vec![SourceKind::Monitor]
    } else {
        options.kinds.clone()
    };
    let mut types = SourceType::empty();
    for kind in wanted {
        match kind {
            SourceKind::Monitor => types |= SourceType::MONITOR,
            SourceKind::Window => types |= SourceType::WINDOW,
            // No portal source type is "an application".
            SourceKind::Application => {}
        }
    }
    types & probe.sources
}

const fn portal_cursor(mode: CursorMode) -> PortalCursor {
    match mode {
        CursorMode::Hidden => PortalCursor::HIDDEN,
        CursorMode::Embedded => PortalCursor::EMBEDDED,
        CursorMode::Metadata => PortalCursor::METADATA,
    }
}

/// How long the consent dialog may stay unanswered before the cast is
/// abandoned (the dialog closed with it).
const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);
/// How often a pending portal call checks whether its caller left.
const CALLER_POLL: Duration = Duration::from_millis(100);

/// What a cast asks the portal for.
struct CastRequest<'a> {
    picker: PickerOptions,
    cursor: CursorRequest,
    consent: &'a ConsentConfig,
    connection: PipeWireConnection,
    caller: &'a Caller,
}

/// Why a pending portal call was given up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Abandoned {
    /// The host call's future was dropped.
    CallerGone,
    /// [`CONSENT_TIMEOUT`] passed.
    TimedOut,
}

impl Abandoned {
    /// Resolves once `caller` left or `timeout` passed.
    async fn watch(caller: &Caller, timeout: Duration) -> Self {
        let deadline = Instant::now() + timeout;
        loop {
            if caller.is_gone() {
                return Self::CallerGone;
            }
            let now = Instant::now();
            if now >= deadline {
                return Self::TimedOut;
            }
            async_io::Timer::after(CALLER_POLL.min(deadline - now)).await;
        }
    }

    fn into_error(self) -> CaptureError {
        match self {
            // Nobody reads it: the host call is gone.
            Self::CallerGone => CaptureError::SessionNotFound,
            Self::TimedOut => CaptureError::protocol(
                ID,
                "Start",
                format!("the consent dialog stayed unanswered for {CONSENT_TIMEOUT:?}"),
            ),
        }
    }

    /// `work`, unless `caller` leaves or `timeout` passes first.
    async fn race<T>(
        work: impl Future<Output = T>,
        caller: &Caller,
        timeout: Duration,
    ) -> Result<T, Self> {
        let mut work = pin!(work);
        let mut watch = pin!(Self::watch(caller, timeout));
        std::future::poll_fn(|cx| {
            if let Poll::Ready(done) = work.as_mut().poll(cx) {
                return Poll::Ready(Ok(done));
            }
            watch.as_mut().poll(cx).map(Err)
        })
        .await
    }
}

/// What a successful handshake yields, before it owns the session.
struct CastParts {
    streams: Vec<PortalStream>,
    restore_token: Option<RestoreToken>,
    restored: bool,
    cursor: CursorMode,
    remote: Option<Remote>,
}

impl CastParts {
    fn with_session(self, session: Session<Screencast>) -> Cast {
        let Self {
            streams,
            restore_token,
            restored,
            cursor,
            remote,
        } = self;
        Cast {
            session,
            streams,
            restore_token,
            restored,
            cursor,
            remote,
        }
    }
}

/// A started cast: the session (closed on drop), its streams and the
/// remote to read them from.
struct Cast {
    session: Session<Screencast>,
    streams: Vec<PortalStream>,
    restore_token: Option<RestoreToken>,
    restored: bool,
    cursor: CursorMode,
    remote: Option<Remote>,
}

impl Cast {
    /// Run the portal handshake (the dialog included). Given up, the
    /// session closed and the dialog with it, once the caller stopped
    /// waiting or [`CONSENT_TIMEOUT`] passed.
    fn start(probe: Probe, request: &CastRequest<'_>) -> Result<Self, CaptureError> {
        pollster::block_on(async {
            let caller = request.caller;
            let created = Abandoned::race(
                async {
                    let proxy = Screencast::new()
                        .await
                        .map_err(|e| portal_error("connect to the ScreenCast portal", &e))?;
                    let session = proxy
                        .create_session(CreateSessionOptions::default())
                        .await
                        .map_err(|e| portal_error("CreateSession", &e))?;
                    Ok::<_, CaptureError>((proxy, session))
                },
                caller,
                CONSENT_TIMEOUT,
            )
            .await;
            let (proxy, session) = created.map_err(Abandoned::into_error)??;
            let negotiated = Abandoned::race(
                Self::negotiate(&proxy, &session, probe, request),
                caller,
                CONSENT_TIMEOUT,
            )
            .await;
            let failed = match negotiated {
                Ok(Ok(cast)) => return Ok(cast.with_session(session)),
                Ok(Err(err)) => err,
                Err(abandoned) => {
                    tracing::debug!(?abandoned, "screen-capture portal: cast given up");
                    abandoned.into_error()
                }
            };
            // A half-set-up cast must not linger on the compositor;
            // closing the session also ends a dialog still open.
            let _closed = session.close().await;
            Err(failed)
        })
    }

    async fn negotiate(
        proxy: &Screencast,
        session: &Session<Screencast>,
        probe: Probe,
        request: &CastRequest<'_>,
    ) -> Result<CastParts, CaptureError> {
        let types = source_types(&request.picker, probe);
        if types.is_empty() {
            return Err(CaptureError::unsatisfiable(
                Some(ID),
                Requirement::Kind(SourceKind::Window),
            ));
        }
        let cursor = select::cursor_mode(request.cursor, &probe.cursor_modes());
        let token = usable_token(request.consent, probe.persists());
        let mut options = SelectSourcesOptions::default()
            .set_sources(types)
            .set_multiple(request.picker.multiple);
        if probe.version >= 2 {
            options = options.set_cursor_mode(portal_cursor(cursor));
        }
        if probe.persists() {
            options = options
                .set_persist_mode(persist_mode(request.consent.persist))
                .set_restore_token(token);
        }
        proxy
            .select_sources(session, options)
            .await
            .and_then(|request| request.response())
            .map_err(|err| portal_error("SelectSources", &err))?;
        let streams = proxy
            .start(session, None, StartCastOptions::default())
            .await
            .and_then(|request| request.response())
            .map_err(|err| portal_error("Start", &err))?;
        if streams.streams().is_empty() {
            return Err(CaptureError::ConsentDenied);
        }
        let remote = match request.connection {
            PipeWireConnection::SessionDaemon => Remote::SessionDaemon,
            PipeWireConnection::PortalRemote => Remote::Fd(
                proxy
                    .open_pipe_wire_remote(session, OpenPipeWireRemoteOptions::default())
                    .await
                    .map_err(|err| portal_error("OpenPipeWireRemote", &err))?,
            ),
        };
        Ok(CastParts {
            restore_token: streams.restore_token().map(|value| RestoreToken {
                value: value.to_owned(),
                backend: ID,
                plugin_version: RESTORE_TOKEN_VERSION,
            }),
            // The portal does not say whether the token skipped the
            // dialog; `restored` means a usable token was offered.
            restored: token.is_some(),
            streams: streams.streams().to_vec(),
            cursor,
            remote: Some(remote),
        })
    }

    /// The streams as plugin sources (logical geometry where the portal
    /// reports it).
    fn parts(&self) -> Vec<Part> {
        self.streams
            .iter()
            .enumerate()
            .map(|(index, stream)| {
                let node = stream.pipe_wire_node_id();
                let kind = match stream.source_type() {
                    Some(t) if t.contains(SourceType::WINDOW) => SourceKind::Window,
                    _ => SourceKind::Monitor,
                };
                let value = stream
                    .id()
                    .map_or_else(|| format!("node-{node}"), str::to_owned);
                let mut source = Source::new(
                    SourceId::new(ID, value),
                    kind,
                    format!("portal stream {index}"),
                );
                if let (Some((x, y)), Some((w, h))) = (stream.position(), stream.size()) {
                    source.geometry = Some(Rect::new(x, y, w.unsigned_abs(), h.unsigned_abs()));
                } else if let Some((w, h)) = stream.size() {
                    source.geometry = Some(Rect::new(0, 0, w.unsigned_abs(), h.unsigned_abs()));
                }
                source.flags.primary = index == 0;
                Part { node, source }
            })
            .collect()
    }

    fn close(&self) {
        // Already closed by the compositor or the person: nothing to do.
        let _closed = pollster::block_on(self.session.close());
    }
}

/// The negotiated stream of a portal capture.
fn negotiated(
    source: Source,
    video: Negotiated,
    cast: &Cast,
    setup: &Setup,
    pool_depth: u8,
    exhaustion: Exhaustion,
) -> NegotiatedStream {
    let dmabuf = video.modifier.is_some();
    NegotiatedStream {
        backend: ID,
        source,
        size: video.size,
        content_rect: Rect::from_size(video.size),
        format: video.format,
        memory: video.memory(),
        modifier: video.modifier,
        tier: video.tier,
        // DMA-BUF and `MemFd` frames leave without a copy of the plugin's;
        // shared memory is the producer's copy.
        copies: if dmabuf {
            Vec::new()
        } else {
            vec![CopyStage::ShmTransport]
        },
        fps: FrameRate::Native,
        source_rate_mhz: video.rate_mhz,
        producer: setup.producer.filter(|_| dmabuf).map(GpuIdentity::Drm),
        // Explicit sync (waited by the plugin, `Ready`) shows per frame;
        // without it a DMA-BUF is implicitly synchronised.
        sync: if dmabuf {
            SyncKind::Implicit
        } else {
            SyncKind::Ready
        },
        pool_depth,
        exhaustion,
        orientation: OrientationSupport::InMeta,
        crop: RegionCrop::InMeta,
        damage: DamageSupport::Rects,
        cursor: cast.cursor,
        colour: Colour::SRGB,
        hdr: false,
        composite: None,
        consent: ConsentModel::PerSessionDialog,
        // The portal does not report what the compositor shows.
        indicator: IndicatorState::NotApplicable,
        border: BorderState::NotApplicable,
        restore_token: cast.restore_token.clone(),
        restored: cast.restored,
        audio: None,
    }
}

fn picker_of(target: &Target) -> PickerOptions {
    match target {
        Target::Picker(options) => options.clone(),
        // A picker-only backend is always handed a picker; several
        // sources mean "let the person pick several monitors".
        Target::Sources { sources, .. } => PickerOptions {
            kinds: vec![SourceKind::Monitor],
            multiple: sources.len() > 1,
        },
    }
}

/// What the compositor imports and renders with, from its
/// `linux-dmabuf` feedback: the producer of the portal's DMA-BUFs. `None`
/// without a Wayland display (an X11 session: shared memory only).
fn compositor() -> (Imports, Option<u64>) {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return (None, None);
    }
    let feedback = super::wayland::Client::connect(ID).and_then(|mut client| client.feedback());
    match feedback {
        Ok(Some(feedback)) => (Some(feedback.accepted), feedback.main_device),
        Ok(None) => (None, None),
        Err(err) => {
            tracing::debug!(%err, "screen-capture portal: no dmabuf feedback; shared memory only");
            (None, None)
        }
    }
}

/// The tiers to negotiate and the producer device. Fails before any
/// dialog when no tier can be served.
fn setup(
    request: &crate::types::FormatRequest,
    exhaustion: Exhaustion,
    cursor: CursorRequest,
) -> Result<Setup, CaptureError> {
    let (imports, producer) = compositor();
    setup_with(request, exhaustion, cursor, &imports, producer)
}

fn setup_with(
    request: &crate::types::FormatRequest,
    exhaustion: Exhaustion,
    cursor: CursorRequest,
    imports: &Imports,
    producer: Option<u64>,
) -> Result<Setup, CaptureError> {
    // CopyToCpu reads shared memory back; an empty request then means it.
    let cpu_only = exhaustion == Exhaustion::CopyToCpu && request.tiers.is_empty();
    let tiers = pods::tiers(request, imports, cpu_only);
    if tiers.is_empty() {
        return Err(CaptureError::unsatisfiable(Some(ID), Requirement::Formats));
    }
    if exhaustion == Exhaustion::CopyToCpu && !pods::offers_shm(&tiers) {
        // No read-back for DMA-BUF (never mapped, no GPU API here).
        return Err(CaptureError::unsatisfiable(
            Some(ID),
            Requirement::ExhaustionReadback,
        ));
    }
    Ok(Setup {
        tiers,
        producer,
        cursor_meta: cursor.mode == CursorMode::Metadata,
    })
}

#[derive(Debug)]
struct Portal;

impl Backend for Portal {
    fn id(&self) -> BackendId {
        ID
    }

    fn availability(&self) -> Availability {
        match probe() {
            Ok(probe) if probe.source_kinds().is_empty() => Availability::NotOnThisSession(
                "the ScreenCast portal offers no monitor or window".to_owned(),
            ),
            Ok(_) => Availability::Ready,
            Err(err) => Availability::NotOnThisSession(format!("no ScreenCast portal: {err}")),
        }
    }

    // The checklist (pixels, frames, exhaustion, sink drop) needs the
    // portal dialog on every run; until it passed on a real session the
    // portal is reachable by name only.
    fn joins_auto(&self) -> bool {
        false
    }

    fn caps(&self) -> Result<BackendCaps, CaptureError> {
        let probe = probe()?;
        let mut caps = BackendCaps::new(ID);
        caps.source_kinds = probe.source_kinds();
        caps.listing = SourceListing::PickerOnly;
        caps.consent = ConsentModel::PerSessionDialog;
        let (imports, producer) = compositor();
        caps.video = pods::RGB_FORMATS
            .into_iter()
            .filter_map(|format| {
                let modifiers = imports
                    .as_ref()
                    .and_then(|i| i.get(&format.drm_fourcc()))
                    .cloned()?;
                Some(FormatCaps {
                    format,
                    memory: BufferMemory::DmaBuf,
                    modifiers,
                    copies: Vec::new(),
                })
            })
            .chain(pods::RGB_FORMATS.into_iter().map(|format| FormatCaps {
                format,
                memory: BufferMemory::Cpu,
                modifiers: Vec::new(),
                copies: vec![CopyStage::ShmTransport],
            }))
            .collect();
        caps.scaling = Scaling::Unsupported;
        caps.region = RegionCrop::InMeta;
        caps.composite = CompositeCaps {
            screenshot: CompositeSupport::CpuOnly,
            streaming: CompositeSupport::Unsupported,
        };
        caps.cursor = probe.cursor_modes();
        caps.damage = DamageSupport::Rects;
        caps.orientation = OrientationSupport::InMeta;
        caps.colour = ColourCaps {
            colours: vec![Colour::SRGB],
            hdr: false,
        };
        caps.audio = AudioCaps::none();
        caps.producer = producer.map(GpuIdentity::Drm);
        if producer.is_some_and(super::gbm::is_nvidia) {
            caps.notes
                .push(crate::types::CapabilityNote::NvidiaImplicitSyncBestEffort);
        }
        caps.sync = vec![SyncKind::Ready, SyncKind::Implicit];
        caps.persistence = if probe.persists() {
            Persistence::Token
        } else {
            Persistence::Unsupported
        };
        caps.pool = PoolCaps {
            min_depth: 2,
            max_depth: 16,
            default_depth: 4,
            exhaustion: vec![
                Exhaustion::Stall,
                Exhaustion::DropNewest,
                Exhaustion::CopyToCpu,
            ],
        };
        // Only when the producer picks shared memory.
        caps.copies = vec![CopyStage::ShmTransport];
        caps.protected_content = ProtectedContent::MaskedUndetectable;
        caps.window = WindowCaptureCaps::default();
        caps.provenance.source_kinds = Provenance::Probe;
        caps.provenance.listing = Provenance::Table;
        // The theoretical set (DMA-BUF with the compositor's modifiers,
        // shared memory). What the producer offers is known only after
        // `Start`: niri's, for one, offers DMA-BUF only.
        caps.provenance.video = Provenance::UnknownUntilNegotiated;
        caps.provenance.cursor = if probe.version >= 2 {
            Provenance::Probe
        } else {
            Provenance::Table
        };
        caps.provenance.damage = Provenance::Table;
        caps.provenance.colour = Provenance::Table;
        caps.provenance.audio = Provenance::Table;
        // The compositor's render device, from its dmabuf feedback; the
        // stream confirms it only by negotiating DMA-BUF.
        caps.provenance.producer = Provenance::UnknownUntilNegotiated;
        caps.provenance.sync = Provenance::Table;
        caps.provenance.persistence = Provenance::Probe;
        Ok(caps)
    }

    fn sources(&self) -> Result<SourceList, CaptureError> {
        Ok(SourceList {
            sources: Vec::new(),
            listing: SourceListing::PickerOnly,
        })
    }

    fn screenshot(&self, plan: &ScreenshotPlan) -> Result<Shot, CaptureError> {
        let setup = setup(&plan.formats, Exhaustion::Stall, plan.cursor)?;
        let probe = probe()?;
        let mut cast = Cast::start(
            probe,
            &CastRequest {
                picker: picker_of(&plan.target),
                cursor: plan.cursor,
                consent: &plan.consent,
                connection: plan.linux.pipewire,
                caller: &plan.caller,
            },
        )?;
        let remote = cast.remote.take().unwrap_or(Remote::SessionDaemon);
        let frames = stream::screenshot(remote, cast.parts(), setup.clone());
        cast.close();
        let frames: Vec<Captured> = frames?;
        let first = frames
            .first()
            .ok_or_else(|| CaptureError::protocol(ID, "screenshot", "no frame captured"))?;
        let video = Negotiated {
            format: first.meta.format,
            size: first.meta.size,
            tier: first.meta.tier,
            rate_mhz: None,
            modifier: first.meta.modifier,
        };
        let negotiated = negotiated(
            first.source.clone(),
            video,
            &cast,
            &setup,
            0,
            Exhaustion::Stall,
        );
        Ok(Shot { frames, negotiated })
    }

    fn open(
        &self,
        plan: StreamPlan,
        out: FrameOut,
    ) -> Result<(Box<dyn BackendSession>, NegotiatedStream), CaptureError> {
        let setup = setup(&plan.formats, plan.exhaustion, plan.cursor)?;
        let probe = probe()?;
        let mut picker = plan.picker.clone().unwrap_or_default();
        picker.multiple = false;
        let mut cast = Cast::start(
            probe,
            &CastRequest {
                picker,
                cursor: plan.cursor,
                consent: &plan.consent,
                connection: plan.linux.pipewire,
                caller: &plan.caller,
            },
        )?;
        let remote = cast.remote.take().unwrap_or(Remote::SessionDaemon);
        let Some(part) = cast.parts().into_iter().next() else {
            cast.close();
            return Err(CaptureError::ConsentDenied);
        };
        let live = Live {
            out,
            pool_depth: plan.pool_depth,
            exhaustion: plan.exhaustion,
        };
        let mut source = part.source.clone();
        let opened = stream::open(remote, part, setup.clone(), live);
        let (thread, video) = match opened {
            Ok(opened) => opened,
            Err(err) => {
                cast.close();
                return Err(err);
            }
        };
        if plan.exhaustion == Exhaustion::CopyToCpu && video.modifier.is_some() {
            // The producer chose DMA-BUF over the listed shared memory:
            // no read-back exists for it.
            drop(thread);
            cast.close();
            return Err(CaptureError::unsatisfiable(
                Some(ID),
                Requirement::ExhaustionReadback,
            ));
        }
        source.physical_size = Some(video.size);
        if let Some(geometry) = source.geometry.filter(|g| g.width > 0) {
            source.scale_permille = Some(video.size.width.saturating_mul(1000) / geometry.width);
        }
        let negotiated = negotiated(
            source,
            video,
            &cast,
            &setup,
            plan.pool_depth,
            plan.exhaustion,
        );
        Ok((
            Box::new(PortalSession {
                thread: Some(thread),
                cast,
            }),
            negotiated,
        ))
    }
}

/// A running portal cast. Dropping it stops the stream, then closes the
/// portal session (which ends the compositor's cast).
struct PortalSession {
    thread: Option<LoopThread>,
    cast: Cast,
}

impl std::fmt::Debug for PortalSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortalSession")
            .field("streams", &self.cast.streams.len())
            .finish_non_exhaustive()
    }
}

impl BackendSession for PortalSession {}

impl Drop for PortalSession {
    fn drop(&mut self) {
        drop(self.thread.take());
        self.cast.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(version: u32) -> Probe {
        Probe {
            version,
            sources: SourceType::MONITOR | SourceType::WINDOW,
            cursors: PortalCursor::HIDDEN | PortalCursor::EMBEDDED | PortalCursor::METADATA,
        }
    }

    fn consent(token: RestoreToken) -> ConsentConfig {
        ConsentConfig {
            restore_token: Some(token),
            ..ConsentConfig::default()
        }
    }

    fn token(backend: BackendId, plugin_version: u32) -> RestoreToken {
        RestoreToken {
            value: "t".into(),
            backend,
            plugin_version,
        }
    }

    #[test]
    fn tokens_from_another_backend_or_version_are_ignored() {
        assert_eq!(
            usable_token(&consent(token(ID, RESTORE_TOKEN_VERSION)), true),
            Some("t")
        );
        assert_eq!(
            usable_token(&consent(token(ID, RESTORE_TOKEN_VERSION)), false),
            None
        );
        assert_eq!(
            usable_token(
                &consent(token(BackendId::LinuxX11, RESTORE_TOKEN_VERSION)),
                true
            ),
            None
        );
        assert_eq!(
            usable_token(&consent(token(ID, RESTORE_TOKEN_VERSION + 1)), true),
            None
        );
        assert_eq!(usable_token(&ConsentConfig::default(), true), None);
    }

    #[test]
    fn cursor_modes_follow_the_portal_and_old_portals_embed() {
        assert_eq!(
            probe(5).cursor_modes(),
            [
                CursorMode::Hidden,
                CursorMode::Embedded,
                CursorMode::Metadata
            ]
        );
        assert_eq!(probe(1).cursor_modes(), [CursorMode::Embedded]);
        assert!(probe(4).persists());
        assert!(!probe(3).persists());
    }

    #[test]
    fn picker_kinds_map_onto_what_the_portal_offers() {
        let monitors_only = Probe {
            sources: SourceType::MONITOR,
            ..probe(5)
        };
        let windows = PickerOptions {
            kinds: vec![SourceKind::Window],
            multiple: false,
        };
        assert_eq!(source_types(&windows, probe(5)), SourceType::WINDOW);
        assert!(source_types(&windows, monitors_only).is_empty());
        assert_eq!(
            source_types(&PickerOptions::default(), probe(5)),
            SourceType::MONITOR
        );
        assert_eq!(
            probe(5).source_kinds(),
            [SourceKind::Monitor, SourceKind::Window]
        );
    }

    #[test]
    fn persist_modes_map_one_to_one() {
        assert_eq!(persist_mode(PersistMode::DoNot), PortalPersist::DoNot);
        assert_eq!(
            persist_mode(PersistMode::WhileRunning),
            PortalPersist::Application
        );
        assert_eq!(
            persist_mode(PersistMode::UntilRevoked),
            PortalPersist::ExplicitlyRevoked
        );
    }

    #[test]
    fn unservable_requests_are_refused_before_the_dialog() {
        use crate::types::{FormatChoice, FormatRequest, FormatTier, PixelFormat};
        let dmabuf = FormatRequest {
            tiers: vec![FormatTier::new(vec![FormatChoice::dmabuf(
                PixelFormat::Bgrx8,
                Vec::new(),
            )])],
        };
        let cursor = CursorRequest::default();
        // No compositor to name modifiers: nothing to offer.
        assert_eq!(
            setup_with(&dmabuf, Exhaustion::Stall, cursor, &None, None).map(|_| ()),
            Err(CaptureError::unsatisfiable(Some(ID), Requirement::Formats))
        );
        let imports = Some(std::collections::HashMap::from([(
            PixelFormat::Bgrx8.drm_fourcc(),
            vec![0x0300_0000_0060_6014],
        )]));
        let setup =
            setup_with(&dmabuf, Exhaustion::Stall, cursor, &imports, Some(7)).expect("dmabuf");
        assert_eq!(setup.producer, Some(7));
        // DMA-BUF has no read-back.
        assert_eq!(
            setup_with(&dmabuf, Exhaustion::CopyToCpu, cursor, &imports, None).map(|_| ()),
            Err(CaptureError::unsatisfiable(
                Some(ID),
                Requirement::ExhaustionReadback
            ))
        );
        // An empty request with CopyToCpu is shared memory.
        let cpu = setup_with(
            &FormatRequest::default(),
            Exhaustion::CopyToCpu,
            cursor,
            &imports,
            None,
        )
        .expect("shm");
        assert!(pods::offers_shm(&cpu.tiers));
    }
}

#[cfg(test)]
mod abandoned_tests {
    use std::time::{Duration, Instant};

    use super::{Abandoned, Caller};

    #[test]
    fn a_pending_portal_call_is_given_up_when_its_caller_leaves() {
        let start = Instant::now();
        let raced = pollster::block_on(Abandoned::race(
            std::future::pending::<()>(),
            &Caller::abandoned(),
            Duration::from_secs(30),
        ));
        assert_eq!(raced, Err(Abandoned::CallerGone));
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(matches!(
            Abandoned::CallerGone.into_error(),
            crate::types::CaptureError::SessionNotFound
        ));
    }

    #[test]
    fn an_unanswered_portal_call_times_out() {
        let raced = pollster::block_on(Abandoned::race(
            std::future::pending::<()>(),
            &Caller::default(),
            Duration::from_millis(150),
        ));
        assert_eq!(raced, Err(Abandoned::TimedOut));
    }

    #[test]
    fn a_finished_portal_call_wins_the_race() {
        let raced = pollster::block_on(Abandoned::race(
            std::future::ready(7),
            &Caller::abandoned(),
            Duration::ZERO,
        ));
        assert_eq!(raced, Ok(7), "work done before the check is kept");
    }
}

/// The PipeWire half of the portal against niri, without a dialog: niri's
/// Mutter ScreenCast API (what its portal calls) hands out a stream node
/// directly. Runs with `ISTMO_SC_TEST_NIRI_SCREENCAST` set
/// (`ISTMO_SC_TEST_CONNECTOR` picks the output, default `DP-3`). niri sends
/// frames on damage only: pick an output whose content changes.
#[cfg(test)]
mod niri_live {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use istmo_core::{Runtime, SinkPolicy};
    use zbus::{OwnedObjectPath, Variant as Value};

    use super::stream::{self, Live, Part};
    use super::*;
    use crate::desktop::output::{EventHub, FrameOut};

    const SERVICE: &str = "org.gnome.Mutter.ScreenCast";

    async fn proxy<'a>(
        conn: &zbus::Connection,
        path: &'a str,
        interface: &'a str,
    ) -> zbus::proxy::Proxy<'a> {
        zbus::proxy::Proxy::new(conn, SERVICE, path, interface)
            .await
            .expect("proxy")
    }

    /// A started niri screencast of `connector`: its connection (kept
    /// alive), session path and PipeWire node.
    async fn record(connector: &str) -> (zbus::Connection, OwnedObjectPath, u32) {
        use zbus::export::futures_core::Stream as _;
        let conn = zbus::Connection::session().await.expect("session bus");
        let screencast = proxy(&conn, "/org/gnome/Mutter/ScreenCast", SERVICE).await;
        let session: OwnedObjectPath = screencast
            .call("CreateSession", &(HashMap::<&str, Value<'_>>::new(),))
            .await
            .expect("CreateSession");
        let session_proxy = proxy(
            &conn,
            session.as_str(),
            "org.gnome.Mutter.ScreenCast.Session",
        )
        .await;
        // cursor-mode 1: embedded.
        let options = HashMap::from([("cursor-mode", Value::U32(1))]);
        let stream: OwnedObjectPath = session_proxy
            .call("RecordMonitor", &(connector, options))
            .await
            .expect("RecordMonitor");
        let stream_proxy =
            proxy(&conn, stream.as_str(), "org.gnome.Mutter.ScreenCast.Stream").await;
        let mut added = stream_proxy
            .receive_signal("PipeWireStreamAdded")
            .await
            .expect("subscribe");
        let () = session_proxy.call("Start", &()).await.expect("Start");
        let message = std::future::poll_fn(|cx| std::pin::Pin::new(&mut added).poll_next(cx))
            .await
            .expect("PipeWireStreamAdded");
        let (node,): (u32,) = message.body().deserialize().expect("node id");
        (conn, session, node)
    }

    fn recv_within(
        sink: &istmo_core::FrameSink<crate::types::FrameMeta>,
        timeout: Duration,
    ) -> Option<istmo_core::NativeFrame<crate::types::FrameMeta>> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(frame) = sink.try_recv() {
                return Some(frame);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn niri_screencast_streams_dma_bufs_without_copies() {
        if std::env::var_os("ISTMO_SC_TEST_NIRI_SCREENCAST").is_none() {
            eprintln!("skipped: set ISTMO_SC_TEST_NIRI_SCREENCAST on a niri session");
            return;
        }
        let connector =
            std::env::var("ISTMO_SC_TEST_CONNECTOR").unwrap_or_else(|_| "DP-3".to_owned());
        let (conn, session, node) = pollster::block_on(record(&connector));
        let setup = setup(
            &crate::types::FormatRequest::default(),
            Exhaustion::Stall,
            CursorRequest::default(),
        )
        .expect("setup");
        let runtime = Runtime::mock().runtime;
        let sink = runtime
            .open_frame_sink::<crate::types::FrameMeta>(SinkPolicy::queue(8))
            .expect("sink");
        let out = FrameOut::new(
            ID,
            runtime.frame_sender(sink.id()).expect("sender"),
            std::sync::Arc::new(EventHub::default()),
            FrameRate::Native,
        );
        let part = Part {
            node,
            source: Source::new(
                SourceId::new(ID, connector.clone()),
                SourceKind::Monitor,
                connector,
            ),
        };
        let live = Live {
            out,
            pool_depth: 4,
            exhaustion: Exhaustion::Stall,
        };
        let (thread, video) =
            stream::open(Remote::SessionDaemon, part, setup, live).expect("negotiated");
        eprintln!("negotiated: {video:?}");
        assert_eq!(video.memory(), BufferMemory::DmaBuf, "niri offers DMA-BUF");
        let mut slots = Vec::new();
        for _ in 0..20 {
            let frame = recv_within(&sink, Duration::from_secs(5)).expect("a frame");
            assert_eq!(frame.meta.memory, BufferMemory::DmaBuf);
            assert!(frame.meta.copied.is_none());
            assert_eq!(frame.meta.modifier, video.modifier);
            let dmabuf = frame.buffer.dmabuf().expect("DMA-BUF backing");
            for plane in &dmabuf.planes {
                let flags = rustix::io::fcntl_getfd(&plane.fd).expect("flags");
                assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
            }
            if !slots.contains(&frame.meta.buffer) {
                slots.push(frame.meta.buffer);
            }
            eprintln!(
                "frame {} sync {:?} orientation {:?} buffer {:?}",
                frame.meta.seq, frame.meta.sync, frame.meta.orientation, frame.meta.buffer
            );
        }
        assert!(slots.len() <= 16, "frames reuse the pool: {slots:?}");
        // Holding every frame stalls at the limit (all but one buffer).
        let mut held = Vec::new();
        while let Some(frame) = recv_within(&sink, Duration::from_secs(2)) {
            held.push(frame);
            assert!(held.len() < 64, "a held pool stalls");
        }
        eprintln!("held at the limit: {}", held.len());
        drop(held);
        assert!(
            recv_within(&sink, Duration::from_secs(5)).is_some(),
            "frames resume once held buffers return"
        );
        drop(thread);
        pollster::block_on(async {
            let session_proxy = proxy(
                &conn,
                session.as_str(),
                "org.gnome.Mutter.ScreenCast.Session",
            )
            .await;
            let _stopped: zbus::Result<()> = session_proxy.call("Stop", &()).await;
        });
    }
}
