//! The ScreenCaptureKit video backend (`macos-sck`): displays, windows
//! and applications from `SCShareableContent`, one `SCStream` per
//! session, frames as retained `IOSurface`s (or their memory, locked
//! read-only), the system picker on 14+, screenshots through
//! `SCScreenshotManager` (14+) or a one-frame stream (12.3–13).
//!
//! Listing and capture need the Screen Recording grant; nothing here
//! prompts for it outside `request_access` (TCC), and a missing grant
//! answers `ConsentRequired` / `AccessRequiresRestart` instead of
//! letting the framework raise its own prompt.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::policy::CapturePolicy;
use crate::select;
use crate::types::{
    AccessScope, AccessState, Availability, BackendCaps, BackendId, BorderState, CaptureError,
    ConsentModel, CursorMode, DamageSupport, Exhaustion, FormatRequest, FrameRate, HdrPolicy,
    IndicatorState, NegotiatedStream, OrientationSupport, OsVersion, OutputSize, PixelFormat, Rect,
    RegionCrop, Requirement, SessionUpdate, Size, Source, SourceId, SourceKind, SourceList,
    SourceListing, SourceSelector, SyncKind,
};

use super::super::backend::{
    Backend, BackendSession, Captured, ScreenshotPlan, Shot, StreamPlan, Target,
};
use super::super::output::FrameOut;
use super::super::sck_map::{self, FourCc, ID, RectF, SourceName};
use super::super::watch::{SourceFeed, WATCH_POLL};
use super::ffi::{
    self, Content, DisplayInfo, Exclusions, Filter, FilterInfo, FilterStyle, Settings, Stream,
    WindowInfo,
};
use super::session::{self, LiveSink, Shape};

pub(crate) fn backend() -> Arc<dyn Backend> {
    Arc::new(SckBackend {
        os: ffi::os_version(),
        requested: AtomicBool::new(false),
    })
}

/// How often a source watch lists again (SCK has no change
/// notification; the diff drops unchanged listings).
const WATCH_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct SckBackend {
    os: OsVersion,
    /// This process asked for the Screen Recording grant
    /// (`CGRequestScreenCaptureAccess`): a missing grant now needs a
    /// relaunch.
    requested: AtomicBool,
}

fn display_source(display: &DisplayInfo) -> Source {
    let mut source = Source::new(
        SourceId::new(ID, SourceName::Display(display.id).value()),
        SourceKind::Monitor,
        if display.main {
            format!("Display {} (main)", display.id)
        } else {
            format!("Display {}", display.id)
        },
    );
    source.geometry = display.frame.rounded();
    source.physical_size = display.pixels;
    source.scale_permille = display.pixels.zip(source.geometry).and_then(|(px, pt)| {
        (pt.width > 0).then(|| {
            u32::try_from(u64::from(px.width) * 1000 / u64::from(pt.width)).unwrap_or(1000)
        })
    });
    source.refresh_mhz = display.refresh_hz.map(sck_map::permille);
    source.flags.primary = display.main;
    // CoreGraphics' vendor / model numbers (EDID manufacturer and product
    // codes), the closest the platform has to make and model.
    source.make = (display.vendor != 0).then(|| format!("{:04x}", display.vendor));
    source.model = (display.model != 0).then(|| format!("{:04x}", display.model));
    source
}

/// The scale of the display under the centre of `frame`.
fn scale_at(displays: &[DisplayInfo], frame: RectF) -> u32 {
    let (cx, cy) = (frame.x + frame.width / 2.0, frame.y + frame.height / 2.0);
    displays
        .iter()
        .find(|d| {
            cx >= d.frame.x
                && cx < d.frame.x + d.frame.width
                && cy >= d.frame.y
                && cy < d.frame.y + d.frame.height
        })
        .or_else(|| displays.iter().find(|d| d.main))
        .map_or(1000, |d| display_source(d).scale())
}

fn window_source(window: &WindowInfo, displays: &[DisplayInfo]) -> Source {
    let app_name = window.app.as_ref().map(|app| app.name.clone());
    let name = window
        .title
        .clone()
        .or_else(|| app_name.clone())
        .unwrap_or_else(|| format!("Window {}", window.id));
    let mut source = Source::new(
        SourceId::new(ID, SourceName::Window(window.id).value()),
        SourceKind::Window,
        name,
    );
    source.app_id = window
        .app
        .as_ref()
        .map(|app| app.bundle_id.clone())
        .filter(|id| !id.is_empty());
    source.pid = window
        .app
        .as_ref()
        .and_then(|app| u32::try_from(app.pid).ok());
    source.geometry = window.frame.rounded();
    let scale = scale_at(displays, window.frame);
    source.scale_permille = Some(scale);
    source.physical_size = source.geometry.map(|g| scaled(g.size(), scale));
    source.flags.occluded = !window.on_screen;
    source.flags.primary = window.active == Some(true);
    source.flags.own_process = source.pid == Some(std::process::id());
    source
}

fn scaled(size: Size, permille: u32) -> Size {
    let scale =
        |v: u32| u32::try_from(u64::from(v) * u64::from(permille) / 1000).unwrap_or(u32::MAX);
    Size::new(scale(size.width), scale(size.height))
}

fn app_source(app: &ffi::AppInfo) -> Source {
    let id = SourceName::Application {
        bundle_id: (!app.bundle_id.is_empty()).then(|| app.bundle_id.clone()),
        pid: Some(app.pid),
    }
    .value();
    let mut source = Source::new(
        SourceId::new(ID, id),
        SourceKind::Application,
        app.name.clone(),
    );
    source.app_id = (!app.bundle_id.is_empty()).then(|| app.bundle_id.clone());
    source.pid = u32::try_from(app.pid).ok();
    source.flags.own_process = source.pid == Some(std::process::id());
    source
}

/// Displays, then application windows (layer 0), then applications.
fn listing(content: &Content, os: OsVersion) -> SourceList {
    let displays = content.displays();
    let mut sources: Vec<Source> = displays.iter().map(display_source).collect();
    sources.extend(
        content
            .windows(os)
            .iter()
            .filter(|w| w.layer == 0 && w.frame.width > 0.0 && w.frame.height > 0.0)
            .map(|w| window_source(w, &displays)),
    );
    sources.extend(
        content
            .applications()
            .iter()
            .filter(|a| !a.name.is_empty())
            .map(app_source),
    );
    SourceList {
        sources,
        listing: SourceListing::Enumerable,
    }
}

/// A capture target, resolved against a fresh listing.
struct Prepared {
    filter: Filter,
    source: Source,
    /// The pixel size SCK delivers natively.
    native: Size,
}

/// The exclusions a session asked for.
fn exclusions(exclude: &[SourceId], own_process: bool) -> Exclusions {
    let mut out = Exclusions::default();
    for id in exclude.iter().filter(|id| id.backend == ID) {
        match SourceName::parse(&id.value) {
            Some(SourceName::Window(window)) => out.windows.push(window),
            Some(SourceName::Application { bundle_id, pid }) => {
                out.bundle_ids.extend(bundle_id);
                out.pids.extend(pid);
            }
            Some(SourceName::Display(_)) | None => {
                tracing::debug!(id = %id.value, "screen-capture: cannot exclude this source");
            }
        }
    }
    if own_process && let Ok(pid) = i32::try_from(std::process::id()) {
        out.pids.push(pid);
    }
    out
}

impl SckBackend {
    fn granted() -> bool {
        ffi::screen_recording_granted()
    }

    /// The error for a capture without the Screen Recording grant.
    fn no_grant(&self) -> CaptureError {
        if self.requested.load(Ordering::Relaxed) {
            CaptureError::AccessRequiresRestart
        } else {
            CaptureError::ConsentRequired
        }
    }

    /// A fresh listing, refusing (instead of prompting) without a grant.
    fn content(&self) -> Result<Content, CaptureError> {
        if !Self::granted() {
            return Err(self.no_grant());
        }
        Content::fetch()
    }

    fn prepare(&self, source: &Source, exclude: &Exclusions) -> Result<Prepared, CaptureError> {
        let content = self.content()?;
        let missing = || CaptureError::SourceNotFound(source.id.clone());
        let named = SourceName::parse(&source.id.value).ok_or_else(missing)?;
        let displays = content.displays();
        let main = displays
            .iter()
            .find(|d| d.main)
            .or_else(|| displays.first())
            .ok_or(CaptureError::SourceLost)?;
        let (filter, source) = match &named {
            SourceName::Display(id) => {
                let display = displays.iter().find(|d| d.id == *id).ok_or_else(missing)?;
                let filter = content.display_filter(*id, exclude).ok_or_else(missing)?;
                (filter, display_source(display))
            }
            SourceName::Window(id) => {
                let window = content
                    .windows(self.os)
                    .into_iter()
                    .find(|w| w.id == *id)
                    .ok_or_else(missing)?;
                let filter = content.window_filter(*id).ok_or_else(missing)?;
                (filter, window_source(&window, &displays))
            }
            SourceName::Application { bundle_id, pid } => {
                let app = content
                    .applications()
                    .into_iter()
                    .find(|a| {
                        bundle_id.as_deref() == Some(a.bundle_id.as_str()) || *pid == Some(a.pid)
                    })
                    .ok_or_else(missing)?;
                let filter = content
                    .application_filter(bundle_id.as_deref(), *pid, main.id, exclude)
                    .ok_or_else(missing)?;
                let mut source = app_source(&app);
                // The application is captured on the main display.
                let display = display_source(main);
                source.geometry = display.geometry;
                source.physical_size = display.physical_size;
                source.scale_permille = display.scale_permille;
                (filter, source)
            }
        };
        let native = source
            .physical_size
            .filter(|s| !s.is_empty())
            .ok_or_else(|| CaptureError::protocol(ID, "prepare", "the source has no size"))?;
        Ok(Prepared {
            filter,
            source,
            native,
        })
    }

    /// The source the picker's choice stands for, and its pixel size.
    fn picked(&self, filter: &Filter) -> (Source, Size) {
        let info = filter.info(self.os).unwrap_or(FilterInfo {
            style: FilterStyle::Unknown,
            content_rect: RectF {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            scale: 1.0,
        });
        let (kind, name) = match info.style {
            FilterStyle::Window => (SourceKind::Window, "picked window"),
            FilterStyle::Application => (SourceKind::Application, "picked application"),
            FilterStyle::Display | FilterStyle::Unknown => (SourceKind::Monitor, "picked display"),
        };
        let mut source = Source::new(SourceId::new(ID, "picker"), kind, name);
        let scale = sck_map::permille(info.scale);
        source.scale_permille = Some(scale);
        source.geometry = info.content_rect.rounded();
        let native = info.content_rect.covering(info.scale).map_or_else(
            || {
                // The picker needs 14, which has the filter's rect: an
                // empty one is unexpected, so say which size was guessed.
                tracing::warn!(
                    "screen-capture: the picked filter has no content rect; capturing at 1920x1080"
                );
                Size::new(1920, 1080)
            },
            Rect::size,
        );
        source.physical_size = Some(native);
        (source, native)
    }

    fn negotiate(&self, formats: &FormatRequest) -> Result<select::Chosen, CaptureError> {
        let caps = sck_map::caps(self.os, true);
        select::negotiate(formats, &select::offers_of(&caps), 0)
            .ok_or_else(|| CaptureError::unsatisfiable(Some(ID), Requirement::Formats))
    }

    fn hdr(&self, policy: HdrPolicy, format: PixelFormat) -> Result<bool, CaptureError> {
        sck_map::hdr(self.os, policy, format)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one SCStreamConfiguration from the plan's independent fields"
    )]
    fn settings(
        &self,
        size: Size,
        format: PixelFormat,
        fps: FrameRate,
        depth: u8,
        cursor: CursorMode,
        scales: bool,
        hdr: bool,
        window: bool,
    ) -> Result<Settings, CaptureError> {
        let fourcc = FourCc::for_format(format)
            .ok_or_else(|| CaptureError::unsatisfiable(Some(ID), Requirement::Formats))?;
        Ok(Settings {
            os: self.os,
            width: usize::try_from(size.width).unwrap_or(usize::MAX),
            height: usize::try_from(size.height).unwrap_or(usize::MAX),
            fourcc: fourcc.0,
            interval: sck_map::frame_interval(fps),
            queue_depth: depth.clamp(sck_map::POOL.min_depth, sck_map::POOL.max_depth),
            shows_cursor: cursor == CursorMode::Embedded,
            scales_to_fit: scales,
            srgb: !hdr && matches!(format, PixelFormat::Bgra8 | PixelFormat::Ar30),
            hdr,
            child_windows: window,
            audio: None,
        })
    }

    /// One frame of `filter` on this OS: `SCScreenshotManager` on 14+, a
    /// one-frame stream before (and when the manager's sample carries no
    /// `IOSurface`-backed pixel buffer, which the frame path needs).
    fn shoot(&self, filter: &Filter, settings: &Settings) -> Result<ffi::Sample, CaptureError> {
        if sck_map::at_least(self.os, sck_map::PICKER) {
            let sample = ffi::screenshot(filter, settings)?;
            if sample.pixel_buffer().is_some() {
                return Ok(sample);
            }
            tracing::debug!(
                "screen-capture: the screenshot has no IOSurface; taking a stream frame"
            );
        }
        session::one_frame(filter, settings)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "every negotiated fact of one session in one place"
    )]
    fn negotiated(
        source: Source,
        size: Size,
        chosen: &select::Chosen,
        cursor: CursorMode,
        hdr: bool,
        pool_depth: u8,
        exhaustion: Exhaustion,
        consent: ConsentModel,
    ) -> NegotiatedStream {
        NegotiatedStream {
            backend: ID,
            source_rate_mhz: source.refresh_mhz,
            source,
            size,
            content_rect: Rect::from_size(size),
            format: chosen.format,
            memory: chosen.memory,
            modifier: None,
            tier: chosen.tier,
            copies: chosen.copies.clone(),
            fps: FrameRate::Native,
            producer: None,
            sync: SyncKind::Ready,
            pool_depth,
            exhaustion,
            orientation: OrientationSupport::AlreadyApplied,
            crop: RegionCrop::InMeta,
            damage: DamageSupport::Rects,
            cursor,
            colour: session::colour_of(chosen.format, hdr),
            hdr,
            composite: None,
            consent,
            indicator: IndicatorState::Shown,
            border: BorderState::NotApplicable,
            restore_token: None,
            restored: false,
            audio: None,
        }
    }

    fn cursor_mode(cursor: crate::types::CursorRequest) -> CursorMode {
        select::cursor_mode(cursor, &[CursorMode::Hidden, CursorMode::Embedded])
    }
}

impl Backend for SckBackend {
    fn id(&self) -> BackendId {
        ID
    }

    fn availability(&self) -> Availability {
        if sck_map::at_least(self.os, sck_map::SCK) {
            Availability::Ready
        } else {
            Availability::OsTooOld(sck_map::SCK)
        }
    }

    // Not verified on a Mac yet: reachable by name (`Fixed`, `Ordered`,
    // the environment override), not through `Auto`.
    fn joins_auto(&self) -> bool {
        false
    }

    fn offers_picker(&self) -> bool {
        sck_map::at_least(self.os, sck_map::PICKER)
    }

    fn access(&self, scope: AccessScope) -> Option<AccessState> {
        (scope == AccessScope::ScreenRecording).then(|| {
            sck_map::screen_recording(Self::granted(), self.requested.load(Ordering::Relaxed))
        })
    }

    fn request_access(
        &self,
        scope: AccessScope,
        _policy: &CapturePolicy,
    ) -> Option<Result<AccessState, CaptureError>> {
        if scope != AccessScope::ScreenRecording {
            return None;
        }
        if Self::granted() {
            return Some(Ok(sck_map::screen_recording(true, false)));
        }
        self.requested.store(true, Ordering::Relaxed);
        let granted = ffi::request_screen_recording();
        Some(Ok(sck_map::screen_recording(granted, true)))
    }

    fn caps(&self) -> Result<BackendCaps, CaptureError> {
        Ok(sck_map::caps(self.os, Self::granted()))
    }

    fn sources(&self) -> Result<SourceList, CaptureError> {
        ffi::pooled(|| {
            if !Self::granted() {
                // Not enumerable before the grant; listing would prompt.
                return Ok(SourceList {
                    sources: Vec::new(),
                    listing: SourceListing::AfterGrant,
                });
            }
            Ok(listing(&Content::fetch()?, self.os))
        })
    }

    fn watch(&self, feed: &mut SourceFeed) -> Result<(), CaptureError> {
        loop {
            if !feed.publish(self.sources()?) {
                return Ok(());
            }
            let due = Instant::now() + WATCH_INTERVAL;
            while Instant::now() < due {
                if feed.is_closed() {
                    return Ok(());
                }
                std::thread::sleep(WATCH_POLL.min(WATCH_INTERVAL));
            }
        }
    }

    fn resolve_special(&self, selector: &SourceSelector) -> Result<Source, CaptureError> {
        ffi::pooled(|| match selector {
            SourceSelector::Focused if sck_map::at_least(self.os, sck_map::WINDOW_ACTIVE) => {
                let content = self.content()?;
                let displays = content.displays();
                content
                    .windows(self.os)
                    .iter()
                    .find(|w| w.layer == 0 && w.active == Some(true))
                    .map(|w| window_source(w, &displays))
                    .ok_or_else(|| {
                        CaptureError::unsatisfiable(Some(ID), Requirement::SourceSelector)
                    })
            }
            _ => Err(CaptureError::unsatisfiable(
                Some(ID),
                Requirement::SourceSelector,
            )),
        })
    }

    fn screenshot(&self, plan: &ScreenshotPlan) -> Result<Shot, CaptureError> {
        ffi::pooled(|| {
            let chosen = self.negotiate(&plan.formats)?;
            let hdr = self.hdr(plan.hdr, chosen.format)?;
            let cursor = Self::cursor_mode(plan.cursor);
            let targets: Vec<(Filter, Source, Size, ConsentModel)> = match &plan.target {
                Target::Sources { sources, .. } => sources
                    .iter()
                    .map(|source| {
                        self.prepare(source, &Exclusions::default())
                            .map(|p| (p.filter, p.source, p.native, ConsentModel::SystemSetting))
                    })
                    .collect::<Result<_, _>>()?,
                Target::Picker(options) => {
                    let filter = ffi::pick(options, || !plan.caller.is_gone())?;
                    let (source, native) = self.picked(&filter);
                    vec![(filter, source, native, ConsentModel::PickerPerSession)]
                }
            };
            let mut frames = Vec::with_capacity(targets.len());
            let mut negotiated = None;
            for (filter, source, native, consent) in targets {
                let window = source.kind == SourceKind::Window;
                let settings = self.settings(
                    native,
                    chosen.format,
                    FrameRate::Native,
                    sck_map::POOL.min_depth,
                    cursor,
                    false,
                    hdr,
                    window,
                )?;
                let sample = self.shoot(&filter, &settings)?;
                let shape = Shape {
                    format: chosen.format,
                    memory: chosen.memory,
                    colour: session::colour_of(chosen.format, hdr),
                    tier: chosen.tier,
                    source_id: source.id.clone(),
                };
                let (meta, buffer) = session::still(&shape, &sample)?;
                if negotiated.is_none() {
                    negotiated = Some(Self::negotiated(
                        source.clone(),
                        meta.size,
                        &chosen,
                        cursor,
                        hdr,
                        1,
                        Exhaustion::Stall,
                        consent,
                    ));
                }
                frames.push(Captured {
                    meta,
                    buffer,
                    source,
                });
            }
            let negotiated =
                negotiated.ok_or_else(|| CaptureError::protocol(ID, "screenshot", "no source"))?;
            Ok(Shot { frames, negotiated })
        })
    }

    fn open(
        &self,
        plan: StreamPlan,
        out: FrameOut,
    ) -> Result<(Box<dyn BackendSession>, NegotiatedStream), CaptureError> {
        ffi::pooled(|| {
            let chosen = self.negotiate(&plan.formats)?;
            let hdr = self.hdr(plan.hdr, chosen.format)?;
            let cursor = Self::cursor_mode(plan.cursor);
            let (filter, source, native, consent) = if let Some(options) = &plan.picker {
                let filter = ffi::pick(options, || !plan.caller.is_gone())?;
                let (source, native) = self.picked(&filter);
                (filter, source, native, ConsentModel::PickerPerSession)
            } else {
                let exclude = exclusions(&plan.exclude, plan.exclude_own_process);
                let prepared = self.prepare(&plan.source, &exclude)?;
                (
                    prepared.filter,
                    prepared.source,
                    prepared.native,
                    ConsentModel::SystemSetting,
                )
            };
            let size = sck_map::output_size(plan.size, native);
            let settings = self.settings(
                size,
                chosen.format,
                plan.fps,
                plan.pool_depth,
                cursor,
                !matches!(plan.size, OutputSize::Native),
                hdr,
                source.kind == SourceKind::Window,
            )?;
            let shape = Shape {
                format: chosen.format,
                memory: chosen.memory,
                colour: session::colour_of(chosen.format, hdr),
                tier: chosen.tier,
                source_id: source.id.clone(),
            };
            let depth = settings.queue_depth;
            let sink = Arc::new(LiveSink::new(out, shape, depth, plan.exhaustion));
            let stream = Stream::start(&filter, &settings, true, false, Arc::clone(&sink) as _)?;
            let negotiated = Self::negotiated(
                source,
                size,
                &chosen,
                cursor,
                hdr,
                depth,
                plan.exhaustion,
                consent,
            );
            let session = SckSession {
                stream,
                sink,
                settings,
                negotiated: negotiated.clone(),
                os: self.os,
                hdr_policy: plan.hdr,
            };
            Ok((Box::new(session) as Box<dyn BackendSession>, negotiated))
        })
    }
}

/// A running ScreenCaptureKit session.
#[derive(Debug)]
struct SckSession {
    stream: Stream,
    sink: Arc<LiveSink>,
    settings: Settings,
    negotiated: NegotiatedStream,
    os: OsVersion,
    /// What the caller asked for HDR, applied again on a format change.
    hdr_policy: HdrPolicy,
}

impl BackendSession for SckSession {
    fn update(&mut self, update: &SessionUpdate) -> Result<NegotiatedStream, CaptureError> {
        let mut settings = self.settings.clone();
        let mut negotiated = self.negotiated.clone();
        let mut reshape = None;
        match update {
            SessionUpdate::Cursor(cursor) => {
                let mode = SckBackend::cursor_mode(*cursor);
                settings.shows_cursor = mode == CursorMode::Embedded;
                negotiated.cursor = mode;
            }
            SessionUpdate::Formats(formats) => {
                let caps = sck_map::caps(self.os, true);
                let chosen = select::negotiate(formats, &select::offers_of(&caps), 0)
                    .ok_or_else(|| CaptureError::unsatisfiable(Some(ID), Requirement::Formats))?;
                let fourcc = FourCc::for_format(chosen.format)
                    .ok_or_else(|| CaptureError::unsatisfiable(Some(ID), Requirement::Formats))?;
                // The session's HDR policy decides again for the new format:
                // an SDR format never keeps an HDR configuration.
                let hdr = sck_map::hdr(self.os, self.hdr_policy, chosen.format)?;
                settings.fourcc = fourcc.0;
                settings.hdr = hdr;
                settings.srgb =
                    !hdr && matches!(chosen.format, PixelFormat::Bgra8 | PixelFormat::Ar30);
                negotiated.hdr = hdr;
                negotiated.format = chosen.format;
                negotiated.memory = chosen.memory;
                negotiated.tier = chosen.tier;
                negotiated.copies.clone_from(&chosen.copies);
                negotiated.colour = session::colour_of(chosen.format, negotiated.hdr);
                reshape = Some(Shape {
                    format: chosen.format,
                    memory: chosen.memory,
                    colour: negotiated.colour,
                    tier: chosen.tier,
                    source_id: negotiated.source.id.clone(),
                });
            }
            SessionUpdate::Fps(_)
            | SessionUpdate::Region(_)
            | SessionUpdate::Pause
            | SessionUpdate::Resume => {
                return Err(CaptureError::unsatisfiable(
                    Some(ID),
                    Requirement::Operation,
                ));
            }
        }
        self.stream.update(&settings)?;
        if let Some(shape) = reshape {
            self.sink.reshape(shape);
        }
        self.settings = settings;
        self.negotiated = negotiated.clone();
        Ok(negotiated)
    }
}
