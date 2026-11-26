//! xdg-desktop-portal ScreenCast + PipeWire.
//!
//! `CreateSession` → `SelectSources` (types, multiple, `cursor_mode` on
//! v ≥ 2, `persist_mode` + `restore_token` on v ≥ 4) → `Start` (the
//! dialog) → one PipeWire stream per chosen source, on the portal's
//! private remote (`OpenPipeWireRemote`) or the session daemon
//! (`LinuxOptions::pipewire`). The token `Start` returns goes back to the
//! app in `NegotiatedStream::restore_token`; the plugin never stores it.
//!
//! Frames are shared memory only (see [`pods`]): the capability report
//! lists CPU formats with `CopyStage::ShmTransport`, so a DMA-BUF-only
//! request is refused up front rather than served with a hidden copy.

mod pods;
mod stream;

use std::sync::Arc;

use ashpd::desktop::screencast::{
    CursorMode as PortalCursor, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions,
    SourceType, StartCastOptions, Stream as PortalStream,
};
use ashpd::desktop::{CreateSessionOptions, PersistMode as PortalPersist, ResponseError, Session};

use crate::select;
use crate::types::{
    AudioCaps, Availability, BackendCaps, BackendId, BorderState, BufferMemory, CaptureError,
    Colour, ColourCaps, CompositeCaps, CompositeSupport, ConsentConfig, ConsentModel, CopyStage,
    CursorMode, CursorRequest, DamageSupport, Exhaustion, FormatCaps, FrameRate, IndicatorState,
    NegotiatedStream, OrientationSupport, PersistMode, Persistence, PickerOptions,
    PipeWireConnection, PoolCaps, ProtectedContent, Provenance, RESTORE_TOKEN_VERSION, Rect,
    RegionCrop, Requirement, RestoreToken, Scaling, Source, SourceId, SourceKind, SourceList,
    SourceListing, SyncKind, WindowCaptureCaps,
};

use self::pods::ShmTier;
use self::stream::{Live, Negotiated, Part};
use super::super::backend::{
    Backend, BackendSession, Captured, ScreenshotPlan, Shot, StreamPlan, Target,
};
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

    /// Cursor modes this backend delivers: the portal's, minus metadata
    /// (its cursor meta is not read). Before v2 the portal takes no
    /// cursor mode and embeds the cursor.
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

/// What a cast asks the portal for.
struct CastRequest<'a> {
    picker: PickerOptions,
    cursor: CursorRequest,
    consent: &'a ConsentConfig,
    connection: PipeWireConnection,
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
    fn start(probe: Probe, request: &CastRequest<'_>) -> Result<Self, CaptureError> {
        pollster::block_on(async {
            let proxy = Screencast::new()
                .await
                .map_err(|e| portal_error("connect to the ScreenCast portal", &e))?;
            let session = proxy
                .create_session(CreateSessionOptions::default())
                .await
                .map_err(|e| portal_error("CreateSession", &e))?;
            match Self::negotiate(proxy, session, probe, request).await {
                Ok(cast) => Ok(cast),
                Err((session, err)) => {
                    // A half-set-up cast must not linger on the compositor.
                    let _closed = session.close().await;
                    Err(err)
                }
            }
        })
    }

    async fn negotiate(
        proxy: Screencast,
        session: Session<Screencast>,
        probe: Probe,
        request: &CastRequest<'_>,
    ) -> Result<Self, (Session<Screencast>, CaptureError)> {
        let types = source_types(&request.picker, probe);
        if types.is_empty() {
            return Err((
                session,
                CaptureError::unsatisfiable(Some(ID), Requirement::Kind(SourceKind::Window)),
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
        let selected = proxy
            .select_sources(&session, options)
            .await
            .and_then(|request| request.response());
        if let Err(err) = selected {
            return Err((session, portal_error("SelectSources", &err)));
        }
        let started = proxy
            .start(&session, None, StartCastOptions::default())
            .await
            .and_then(|request| request.response());
        let streams = match started {
            Ok(streams) => streams,
            Err(err) => return Err((session, portal_error("Start", &err))),
        };
        if streams.streams().is_empty() {
            return Err((session, CaptureError::ConsentDenied));
        }
        let remote = match request.connection {
            PipeWireConnection::SessionDaemon => Remote::SessionDaemon,
            PipeWireConnection::PortalRemote => {
                match proxy
                    .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
                    .await
                {
                    Ok(fd) => Remote::Fd(fd),
                    Err(err) => return Err((session, portal_error("OpenPipeWireRemote", &err))),
                }
            }
        };
        Ok(Self {
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
            session,
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
    pool_depth: u8,
    exhaustion: Exhaustion,
) -> NegotiatedStream {
    NegotiatedStream {
        backend: ID,
        source,
        size: video.size,
        content_rect: Rect::from_size(video.size),
        format: video.format,
        memory: BufferMemory::Cpu,
        modifier: None,
        tier: video.tier,
        copies: vec![CopyStage::ShmTransport],
        fps: FrameRate::Native,
        source_rate_mhz: video.rate_mhz,
        producer: None,
        sync: SyncKind::Ready,
        pool_depth,
        exhaustion,
        orientation: OrientationSupport::Unsupported,
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

fn shm_tiers(request: &crate::types::FormatRequest) -> Result<Vec<ShmTier>, CaptureError> {
    let tiers = pods::shm_tiers(request);
    if tiers.is_empty() {
        Err(CaptureError::unsatisfiable(Some(ID), Requirement::Formats))
    } else {
        Ok(tiers)
    }
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
        caps.video = pods::SHM_FORMATS
            .into_iter()
            .map(|format| FormatCaps {
                format,
                memory: BufferMemory::Cpu,
                modifiers: Vec::new(),
                copies: vec![CopyStage::ShmTransport],
            })
            .collect();
        caps.scaling = Scaling::Unsupported;
        caps.region = RegionCrop::InMeta;
        caps.composite = CompositeCaps {
            screenshot: CompositeSupport::CpuOnly,
            streaming: CompositeSupport::Unsupported,
        };
        caps.cursor = probe.cursor_modes();
        caps.damage = DamageSupport::Rects;
        caps.orientation = OrientationSupport::Unsupported;
        caps.colour = ColourCaps {
            colours: vec![Colour::SRGB],
            hdr: false,
        };
        caps.audio = AudioCaps::none();
        caps.producer = None;
        caps.sync = vec![SyncKind::Ready];
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
        caps.copies = vec![CopyStage::ShmTransport];
        caps.protected_content = ProtectedContent::MaskedUndetectable;
        caps.window = WindowCaptureCaps::default();
        caps.provenance.source_kinds = Provenance::Probe;
        caps.provenance.listing = Provenance::Table;
        // The set is the implementation's (shared memory, packed RGB);
        // which one the producer picks is negotiated.
        caps.provenance.video = Provenance::Table;
        caps.provenance.cursor = if probe.version >= 2 {
            Provenance::Probe
        } else {
            Provenance::Table
        };
        caps.provenance.damage = Provenance::Table;
        caps.provenance.colour = Provenance::Table;
        caps.provenance.audio = Provenance::Table;
        // Shared memory never names its GPU.
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
        let tiers = shm_tiers(&plan.formats)?;
        let probe = probe()?;
        let mut cast = Cast::start(
            probe,
            &CastRequest {
                picker: picker_of(&plan.target),
                cursor: plan.cursor,
                consent: &plan.consent,
                connection: plan.linux.pipewire,
            },
        )?;
        let remote = cast.remote.take().unwrap_or(Remote::SessionDaemon);
        let frames = stream::screenshot(remote, cast.parts(), tiers);
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
        };
        let negotiated = negotiated(first.source.clone(), video, &cast, 0, Exhaustion::Stall);
        Ok(Shot { frames, negotiated })
    }

    fn open(
        &self,
        plan: StreamPlan,
        out: FrameOut,
    ) -> Result<(Box<dyn BackendSession>, NegotiatedStream), CaptureError> {
        let tiers = shm_tiers(&plan.formats)?;
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
        let opened = stream::open(remote, part, tiers, live);
        let (thread, video) = match opened {
            Ok(opened) => opened,
            Err(err) => {
                cast.close();
                return Err(err);
            }
        };
        source.physical_size = Some(video.size);
        if let Some(geometry) = source.geometry.filter(|g| g.width > 0) {
            source.scale_permille = Some(video.size.width.saturating_mul(1000) / geometry.width);
        }
        let negotiated = negotiated(source, video, &cast, plan.pool_depth, plan.exhaustion);
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
    fn metadata_cursors_are_not_offered_and_old_portals_embed() {
        assert_eq!(
            probe(5).cursor_modes(),
            [CursorMode::Hidden, CursorMode::Embedded]
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
    fn dmabuf_only_requests_are_refused_by_name() {
        let request = crate::types::FormatRequest {
            tiers: vec![crate::types::FormatTier::new(vec![
                crate::types::FormatChoice::dmabuf(crate::types::PixelFormat::Bgrx8, Vec::new()),
            ])],
        };
        assert_eq!(
            shm_tiers(&request),
            Err(CaptureError::unsatisfiable(Some(ID), Requirement::Formats))
        );
    }
}
