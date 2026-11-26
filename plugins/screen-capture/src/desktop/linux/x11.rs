//! X11 backend (`linux-x11`): XShm reads of the root window (monitors)
//! or of a Composite window pixmap (windows), XDamage for damage and
//! `OnDamage`, XFixes for the cursor. Pure Rust (`x11rb`, no libxcb).
//!
//! Only chosen when there is no Wayland session: a `DISPLAY` inside a
//! Wayland session is XWayland, which would capture X clients only.
//! Buffers are CPU memory the server copied into
//! (`CopyStage::ShmTransport`); DRI3 live buffers are not offered.

mod conn;
mod grab;
mod session;

use std::sync::Arc;

use istmo_core::NativeBuffer;

use crate::select::{self, Offer};
use crate::types::{
    Availability, BackendCaps, BackendId, BorderState, BufferIdentity, BufferMemory,
    CapsProvenance, CaptureError, Colour, CompositeCaps, CompositeSupport, ConsentModel, CopyStage,
    CursorMode, CursorState, Damage, DamageSupport, Exhaustion, FormatCaps, FormatRequest,
    FrameMeta, FrameRate, IndicatorState, NegotiatedStream, OrientationSupport, PixelFormat,
    PlaneLayout, Point, PoolCaps, ProtectedContent, Provenance, PtsClock, Rect, RegionCrop,
    Requirement, Scaling, Source, SourceKind, SourceList, SourceListing, SourceSelector, SyncKind,
    WindowCaptureCaps,
};

use super::super::backend::{
    Backend, BackendSession, Captured, ScreenshotPlan, Shot, StreamPlan, Target,
};
use super::super::clock;
use super::super::output::FrameOut;

use self::conn::{ID, Target as XTarget, X11};
use self::grab::{Cursor, Grab, Grabber};

pub(crate) fn backend() -> Arc<dyn Backend> {
    Arc::new(X11Backend)
}

/// Pool limits: CPU buffers are cheap, but each one is a full frame.
const POOL: PoolCaps = PoolCaps {
    min_depth: 2,
    max_depth: 8,
    default_depth: 3,
    exhaustion: Vec::new(),
};

#[derive(Debug)]
struct X11Backend;

/// The formats this server delivers: the root depth, plus ARGB for
/// 32-bit windows when the screen has such a visual.
fn formats(x: &X11) -> Vec<FormatCaps> {
    let mut formats: Vec<PixelFormat> = Vec::new();
    let lsb = x.conn_lsb();
    if let Some(format) = grab::pixel_format(x.root_depth, 32, lsb) {
        formats.push(format);
    }
    if x.argb_visual
        && let Some(format) = grab::pixel_format(32, 32, lsb)
        && !formats.contains(&format)
    {
        formats.push(format);
    }
    formats
        .into_iter()
        .map(|format| FormatCaps {
            format,
            memory: BufferMemory::Cpu,
            modifiers: Vec::new(),
            copies: vec![CopyStage::ShmTransport],
        })
        .collect()
}

fn offers(x: &X11) -> Vec<Offer> {
    formats(x)
        .into_iter()
        .map(|f| Offer {
            format: f.format,
            memory: f.memory,
            modifiers: f.modifiers,
            copies: f.copies,
        })
        .collect()
}

fn cursor_modes(x: &X11) -> Vec<CursorMode> {
    if x.ext.xfixes {
        vec![
            CursorMode::Hidden,
            CursorMode::Embedded,
            CursorMode::Metadata,
        ]
    } else {
        vec![CursorMode::Hidden]
    }
}

fn caps_of(x: &X11) -> BackendCaps {
    let mut caps = BackendCaps::new(ID);
    caps.source_kinds = vec![SourceKind::Monitor, SourceKind::Window];
    caps.listing = SourceListing::Enumerable;
    caps.consent = ConsentModel::NoneNeeded;
    caps.video = formats(x);
    caps.max_size = Some(x.root_size);
    caps.scaling = Scaling::Unsupported;
    caps.region = RegionCrop::InMeta;
    caps.composite = CompositeCaps {
        screenshot: CompositeSupport::CpuOnly,
        streaming: CompositeSupport::Unsupported,
    };
    caps.cursor = cursor_modes(x);
    caps.damage = if x.ext.damage {
        DamageSupport::Rects
    } else {
        DamageSupport::Unsupported
    };
    // RandR rotation is applied in the root window already.
    caps.orientation = OrientationSupport::AlreadyApplied;
    caps.sync = vec![SyncKind::Ready];
    caps.pool = PoolCaps {
        exhaustion: vec![
            Exhaustion::Stall,
            Exhaustion::DropNewest,
            Exhaustion::CopyToCpu,
        ],
        ..POOL
    };
    caps.copies = vec![CopyStage::ShmTransport];
    // X11 has no content protection: nothing is ever masked, so there
    // is nothing to report or to detect.
    caps.protected_content = ProtectedContent::Reported;
    caps.window = WindowCaptureCaps {
        occluded: x.ext.composite,
        minimised: false,
        child_popups: false,
        border: false,
    };
    caps.provenance = CapsProvenance {
        source_kinds: Provenance::Probe,
        listing: Provenance::Probe,
        video: Provenance::Probe,
        cursor: Provenance::Probe,
        damage: Provenance::Probe,
        colour: Provenance::Table,
        audio: Provenance::Table,
        producer: Provenance::Table,
        sync: Provenance::Table,
        persistence: Provenance::Table,
    };
    caps
}

impl X11 {
    fn conn_lsb(&self) -> bool {
        use x11rb::connection::Connection as _;
        self.conn.setup().image_byte_order == x11rb::protocol::xproto::ImageOrder::LSB_FIRST
    }
}

/// The cursor as the frame should carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorPlan {
    Hidden,
    Embedded,
    Metadata,
}

impl From<CursorMode> for CursorPlan {
    fn from(mode: CursorMode) -> Self {
        match mode {
            CursorMode::Hidden => Self::Hidden,
            CursorMode::Embedded => Self::Embedded,
            CursorMode::Metadata => Self::Metadata,
        }
    }
}

/// Apply the cursor plan to a grab whose top-left is `origin` on the
/// root window.
fn apply_cursor(x: &X11, plan: CursorPlan, grab: &mut Grab, origin: Point) -> Option<CursorState> {
    if plan == CursorPlan::Hidden {
        return None;
    }
    let cursor = Cursor::current(x)?;
    match plan {
        CursorPlan::Hidden => None,
        CursorPlan::Embedded => {
            if grab.format.bytes_per_pixel() == 4 && grab.format != PixelFormat::Xr30 {
                cursor.blend_into(&mut grab.bytes, grab.stride, grab.size, origin);
            }
            None
        }
        CursorPlan::Metadata => Some(CursorState {
            position: Point {
                x: cursor.position.x - origin.x,
                y: cursor.position.y - origin.y,
            },
            hotspot: cursor.hotspot,
            visible: true,
        }),
    }
}

/// Read one frame of `source`.
fn capture(
    x: &X11,
    grabber: &mut Grabber,
    source: &Source,
    cursor: CursorPlan,
    reuse: Vec<u8>,
) -> Result<(Grab, Option<CursorState>), CaptureError> {
    let (mut grab, origin) = match X11::target(source)? {
        XTarget::Monitor(rect) => (
            grabber.grab(x, x.root, x.root_depth, rect, reuse)?,
            Point {
                x: rect.x,
                y: rect.y,
            },
        ),
        XTarget::Window(window) => {
            let at = x.window_rect(window)?;
            (
                grabber.grab_window(x, window, reuse)?,
                Point { x: at.x, y: at.y },
            )
        }
    };
    let state = apply_cursor(x, cursor, &mut grab, origin);
    Ok((grab, state))
}

fn meta_for(
    source: &Source,
    grab: &Grab,
    pts_ns: u64,
    damage: Damage,
    cursor: Option<CursorState>,
) -> FrameMeta {
    FrameMeta {
        seq: 0,
        pts_ns,
        pts_clock: PtsClock::Monotonic,
        wall_us: 0,
        size: grab.size,
        content_rect: Rect::from_size(grab.size),
        format: grab.format,
        memory: BufferMemory::Cpu,
        planes: vec![PlaneLayout {
            offset: 0,
            stride: grab.stride,
            size: u64::from(grab.stride) * u64::from(grab.size.height),
        }],
        modifier: None,
        buffer: BufferIdentity::default(),
        tier: 0,
        copied: None,
        producer: None,
        colour: Colour::SRGB,
        hdr: None,
        orientation: source.orientation,
        damage,
        cursor,
        sync: SyncKind::Ready,
        scale_permille: 1000,
        source_id: source.id.clone(),
        repeat: false,
        skipped_before: 0,
        protected_content: false,
        composite: None,
    }
}

fn negotiated(
    x: &X11,
    source: &Source,
    grab: &Grab,
    cursor: CursorMode,
    tier: u8,
    pool_depth: u8,
    exhaustion: Exhaustion,
) -> NegotiatedStream {
    NegotiatedStream {
        backend: ID,
        source: source.clone(),
        size: grab.size,
        content_rect: Rect::from_size(grab.size),
        format: grab.format,
        memory: BufferMemory::Cpu,
        modifier: None,
        tier,
        copies: vec![CopyStage::ShmTransport],
        fps: FrameRate::Native,
        source_rate_mhz: source.refresh_mhz,
        producer: None,
        sync: SyncKind::Ready,
        pool_depth,
        exhaustion,
        orientation: OrientationSupport::AlreadyApplied,
        crop: RegionCrop::InMeta,
        damage: if x.ext.damage {
            DamageSupport::Rects
        } else {
            DamageSupport::Unsupported
        },
        cursor,
        colour: Colour::SRGB,
        hdr: false,
        composite: None,
        consent: ConsentModel::NoneNeeded,
        indicator: IndicatorState::NotApplicable,
        border: BorderState::NotApplicable,
        restore_token: None,
        restored: false,
        audio: None,
    }
}

/// The tier the request gets on this server.
fn choose_tier(x: &X11, formats: &FormatRequest) -> Result<u8, CaptureError> {
    select::negotiate(formats, &offers(x), 0)
        .map(|chosen| chosen.tier)
        .ok_or_else(|| CaptureError::unsatisfiable(Some(ID), Requirement::Formats))
}

fn not_on_this_session() -> Option<Availability> {
    if super::wayland_session() {
        // XWayland: X11 capture would only see X clients, black elsewhere.
        return Some(Availability::NotOnThisSession("XWayland only".to_owned()));
    }
    if std::env::var_os("DISPLAY").is_none() {
        return Some(Availability::NotOnThisSession("no DISPLAY".to_owned()));
    }
    None
}

impl Backend for X11Backend {
    fn id(&self) -> BackendId {
        ID
    }

    fn availability(&self) -> Availability {
        if let Some(why) = not_on_this_session() {
            return why;
        }
        match X11::connect() {
            Ok(_) => Availability::Ready,
            Err(err) => Availability::NotOnThisSession(err.to_string()),
        }
    }

    fn caps(&self) -> Result<BackendCaps, CaptureError> {
        let x = X11::connect()?;
        Ok(caps_of(&x))
    }

    fn sources(&self) -> Result<SourceList, CaptureError> {
        let x = X11::connect()?;
        let mut sources = x.monitors()?;
        sources.extend(x.windows()?);
        Ok(SourceList {
            sources,
            listing: SourceListing::Enumerable,
        })
    }

    fn resolve_special(&self, selector: &SourceSelector) -> Result<Source, CaptureError> {
        let x = X11::connect()?;
        match selector {
            SourceSelector::Focused => x.focused(),
            // The monitor the pointer is on.
            SourceSelector::UnderCursor => x.under_cursor(),
            _ => Err(CaptureError::unsatisfiable(
                Some(ID),
                Requirement::SourceSelector,
            )),
        }
    }

    fn screenshot(&self, plan: &ScreenshotPlan) -> Result<Shot, CaptureError> {
        let Target::Sources { sources, .. } = &plan.target else {
            return Err(CaptureError::unsatisfiable(Some(ID), Requirement::Picker));
        };
        let x = X11::connect()?;
        let tier = choose_tier(&x, &plan.formats)?;
        let mode = select::cursor_mode(plan.cursor, &cursor_modes(&x));
        let mut grabber = Grabber::default();
        let mut frames = Vec::with_capacity(sources.len());
        let mut first = None;
        for source in sources {
            let pts = clock::monotonic_ns();
            let (grab, cursor) = capture(&x, &mut grabber, source, mode.into(), Vec::new())?;
            let mut meta = meta_for(source, &grab, pts, Damage::Full, cursor);
            meta.tier = tier;
            if first.is_none() {
                first = Some(negotiated(
                    &x,
                    source,
                    &grab,
                    mode,
                    tier,
                    1,
                    Exhaustion::Stall,
                ));
            }
            frames.push(Captured {
                meta,
                buffer: NativeBuffer::from_bytes(grab.bytes),
                source: source.clone(),
            });
        }
        grabber.release(&x);
        let negotiated =
            first.ok_or_else(|| CaptureError::protocol(ID, "screenshot", "no source"))?;
        Ok(Shot { frames, negotiated })
    }

    fn open(
        &self,
        plan: StreamPlan,
        out: FrameOut,
    ) -> Result<(Box<dyn BackendSession>, NegotiatedStream), CaptureError> {
        if plan.picker.is_some() {
            return Err(CaptureError::unsatisfiable(Some(ID), Requirement::Picker));
        }
        let x = X11::connect()?;
        let tier = choose_tier(&x, &plan.formats)?;
        let mode = select::cursor_mode(plan.cursor, &cursor_modes(&x));
        let (session, negotiated) = session::start(x, &plan, out, tier, mode)?;
        Ok((Box::new(session), negotiated))
    }

    fn joins_auto(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Size;

    #[test]
    fn cursor_plans_follow_the_mode() {
        assert_eq!(CursorPlan::from(CursorMode::Metadata), CursorPlan::Metadata);
        assert_eq!(CursorPlan::from(CursorMode::Hidden), CursorPlan::Hidden);
    }

    #[test]
    fn frames_say_how_they_were_made() {
        let source = Source::new(
            crate::types::SourceId::new(ID, "monitor:screen"),
            SourceKind::Monitor,
            "screen",
        );
        let grab = Grab {
            size: Size::new(4, 2),
            stride: 16,
            format: PixelFormat::Bgrx8,
            bytes: vec![0; 32],
        };
        let meta = meta_for(&source, &grab, 5, Damage::Full, None);
        assert_eq!(meta.planes[0].size, 32);
        assert_eq!(meta.memory, BufferMemory::Cpu);
        assert_eq!(meta.sync, SyncKind::Ready);
        assert_eq!(meta.pts_clock, PtsClock::Monotonic);
    }
}
