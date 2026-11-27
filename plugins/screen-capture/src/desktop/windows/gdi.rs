//! GDI backend (`windows-gdi`), the last resort: `BitBlt` of the screen
//! (monitors) and `PrintWindow(PW_RENDERFULLCONTENT)` (windows, occluded
//! ones too) into DIB sections, CPU memory only.
//!
//! Each frame is the DIB section GDI drew into (the producer's copy into
//! CPU memory, `CopyStage::ShmTransport`), pooled like the X11 shared
//! memory segments: a slot stays leased while any clone of its frame
//! lives. No damage, no rotation (the desktop arrives upright). Content
//! protected with `WDA_EXCLUDEFROMCAPTURE` or DRM comes out black, and
//! GDI does not say so (`ProtectedContent::MaskedUndetectable`).

#![allow(
    unsafe_code,
    reason = "Win32 GDI FFI: DIB sections, BitBlt, PrintWindow, cursor drawing"
)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC,
    SRCCOPY, SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetIconInfo, GetWindowRect,
    GetWindowThreadProcessId, HICON, ICONINFO, IsHungAppWindow, PW_RENDERFULLCONTENT,
};

use crate::pool::{HoldError, PoolAccounting};
use crate::select::{self, Offer};
use crate::types::{
    Availability, BackendCaps, BackendId, BorderState, BufferMemory, CapsProvenance, CaptureError,
    CaptureEvent, CompositeCaps, CompositeSupport, ConsentModel, CopyStage, CursorMode,
    CursorState, Damage, DamageSupport, Exhaustion, FormatCaps, FormatRequest, NegotiatedStream,
    OrientationSupport, PixelFormat, Point, PoolCaps, ProtectedContent, Provenance, Rect,
    RegionCrop, Requirement, Scaling, SessionUpdate, Size, Source, SourceCapture, SourceKind,
    SourceList, SourceListing, SourceSelector, Strength, WindowCaptureCaps,
};

use super::super::backend::{
    Backend, BackendSession, Captured, ScreenshotPlan, Shot, StreamPlan, Target,
};
use super::super::output::{Delivery, FrameOut};
use super::super::watch::SourceFeed;
use super::common::{FrameFacts, StreamFacts, frame_meta, negotiated, period};
use super::listing::{self, Target as WinTarget};
use super::os::{self, PhysicalPixels};

const ID: BackendId = BackendId::WindowsGdi;

pub(crate) fn backend() -> Arc<dyn Backend> {
    Arc::new(GdiBackend)
}

/// CPU buffers are cheap, but each one is a full frame.
const POOL: PoolCaps = PoolCaps {
    min_depth: 2,
    max_depth: 8,
    default_depth: 3,
    exhaustion: Vec::new(),
};
/// How long a capture thread sleeps at most before it looks at its flags.
const IDLE_WAIT: Duration = Duration::from_millis(100);
/// How often the source is re-read (moved, resized, minimised, gone).
const SOURCE_CHECK: Duration = Duration::from_millis(250);
/// How long dropping a session waits for its thread.
const JOIN_WAIT: Duration = Duration::from_secs(2);
/// How long `PrintWindow` of another process's window may take: its
/// owner thread draws it, and may be busy or hung.
const PRINT_WAIT: Duration = Duration::from_millis(500);
/// Frames in a row a window may miss (hung, or slower than
/// [`PRINT_WAIT`]) before its stream fails.
const PRINT_MISSES: u32 = 20;

const FORMAT: PixelFormat = PixelFormat::Bgrx8;
const CURSOR_MODES: [CursorMode; 3] = [
    CursorMode::Hidden,
    CursorMode::Embedded,
    CursorMode::Metadata,
];

fn gdi_err(step: &str, message: impl std::fmt::Display) -> CaptureError {
    CaptureError::protocol(ID, step, message)
}

/// A top-down 32-bit DIB section: the pixels GDI draws into, read in
/// place as the frame's bytes.
#[derive(Debug)]
struct Dib {
    bitmap: isize,
    bits: usize,
    size: Size,
}

impl Dib {
    fn new(size: Size) -> Result<Self, CaptureError> {
        if size.is_empty() {
            return Err(gdi_err("CreateDIBSection", "empty source"));
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: u32::try_from(std::mem::size_of::<BITMAPINFOHEADER>()).unwrap_or(40),
                biWidth: i32::try_from(size.width).map_err(|e| gdi_err("CreateDIBSection", e))?,
                // Negative: rows top-down.
                biHeight: -i32::try_from(size.height)
                    .map_err(|e| gdi_err("CreateDIBSection", e))?,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..BITMAPINFOHEADER::default()
            },
            ..BITMAPINFO::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        // SAFETY: `info` describes a 32-bit top-down DIB; `bits` receives
        // the pointer to its pixels, owned by the returned bitmap.
        let bitmap = unsafe {
            CreateDIBSection(
                None,
                &raw const info,
                DIB_RGB_COLORS,
                &raw mut bits,
                None,
                0,
            )
        }
        .map_err(|e| CaptureError::os(ID, "CreateDIBSection", i64::from(e.code().0)))?;
        if bits.is_null() {
            // SAFETY: the bitmap was created above and is deleted once.
            let _deleted = unsafe { DeleteObject(HGDIOBJ(bitmap.0)) };
            return Err(gdi_err("CreateDIBSection", "no pixel memory"));
        }
        Ok(Self {
            bitmap: bitmap.0 as isize,
            bits: bits as usize,
            size,
        })
    }

    const fn stride(&self) -> u32 {
        self.size.width * 4
    }

    fn len(&self) -> usize {
        usize::try_from(u64::from(self.stride()) * u64::from(self.size.height)).unwrap_or(0)
    }

    const fn handle(&self) -> HBITMAP {
        HBITMAP(self.bitmap as *mut c_void)
    }
}

impl AsRef<[u8]> for Dib {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: `bits` points to the DIB section's `len()` bytes, alive
        // until the bitmap is deleted in `drop`. GDI writes into a pooled
        // DIB only while no frame of it is leased (the pool's rule), and
        // `GdiFlush` ran after the last write.
        unsafe { std::slice::from_raw_parts(self.bits as *const u8, self.len()) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: the bitmap is owned by this value and deleted once; no
        // DC has it selected any more.
        let _deleted = unsafe { DeleteObject(HGDIOBJ(self.handle().0)) };
    }
}

/// The screen DC for the duration of one grab.
struct ScreenDc(HDC);

impl ScreenDc {
    fn get() -> Result<Self, CaptureError> {
        // SAFETY: `GetDC(NULL)` returns the screen DC, released in drop.
        let dc = unsafe { GetDC(None) };
        if dc.is_invalid() {
            return Err(gdi_err("GetDC", "no screen DC"));
        }
        Ok(Self(dc))
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: releases the DC `get` obtained.
        let _released = unsafe { ReleaseDC(None, self.0) };
    }
}

/// A memory DC with a DIB selected into it, restored and deleted on
/// drop.
struct MemDc {
    dc: HDC,
    previous: HGDIOBJ,
}

impl MemDc {
    fn with(screen: &ScreenDc, dib: &Dib) -> Result<Self, CaptureError> {
        // SAFETY: `screen` is a live DC; the memory DC is deleted in drop.
        let dc = unsafe { CreateCompatibleDC(Some(screen.0)) };
        if dc.is_invalid() {
            return Err(gdi_err("CreateCompatibleDC", "no memory DC"));
        }
        // SAFETY: `dc` and the bitmap are live; the previous object is
        // selected back in drop.
        let previous = unsafe { SelectObject(dc, HGDIOBJ(dib.handle().0)) };
        Ok(Self { dc, previous })
    }
}

impl Drop for MemDc {
    fn drop(&mut self) {
        // SAFETY: restores the DC's original bitmap so the DIB can be
        // deleted later.
        let _dib = unsafe { SelectObject(self.dc, self.previous) };
        // SAFETY: deletes the DC `with` created.
        let _deleted = unsafe { DeleteDC(self.dc) };
    }
}

/// What one grab reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Area {
    /// A monitor's desktop rectangle.
    Screen(Rect),
    /// A window, with its window rectangle.
    Window(isize, Rect),
}

impl Area {
    const fn rect(self) -> Rect {
        match self {
            Self::Screen(rect) | Self::Window(_, rect) => rect,
        }
    }
}

fn window_rect(hwnd: isize) -> Result<Rect, CaptureError> {
    let _pixels = PhysicalPixels::enter();
    let mut rect = RECT::default();
    // SAFETY: `rect` is a live out value; a stale handle fails.
    unsafe { GetWindowRect(listing::hwnd(hwnd), &raw mut rect) }
        .map_err(|_| CaptureError::SourceLost)?;
    Ok(Rect::new(
        rect.left,
        rect.top,
        rect.right.saturating_sub(rect.left).unsigned_abs(),
        rect.bottom.saturating_sub(rect.top).unsigned_abs(),
    ))
}

/// Where `source` is now.
fn area_of(source: &Source) -> Result<Area, CaptureError> {
    match listing::target_of(ID, source)? {
        WinTarget::Monitor(device) => listing::monitor(ID, &device, false)
            .map(|m| Area::Screen(m.rect))
            .ok_or(CaptureError::SourceLost),
        WinTarget::Window(hwnd) => {
            if !listing::window_exists(hwnd) {
                return Err(CaptureError::SourceLost);
            }
            Ok(Area::Window(hwnd, window_rect(hwnd)?))
        }
    }
}

/// The cursor now: its image, top-left position and hotspot.
struct Cursor {
    icon: HICON,
    position: Point,
    hotspot: Point,
}

fn cursor() -> Option<Cursor> {
    let _pixels = PhysicalPixels::enter();
    let mut info = CURSORINFO {
        cbSize: u32::try_from(std::mem::size_of::<CURSORINFO>()).unwrap_or(0),
        ..CURSORINFO::default()
    };
    // SAFETY: `info` is a live CURSORINFO whose cbSize says so.
    unsafe { GetCursorInfo(&raw mut info) }.ok()?;
    if info.flags.0 & CURSOR_SHOWING.0 == 0 || info.hCursor.is_invalid() {
        return None;
    }
    let icon = HICON(info.hCursor.0);
    let mut icon_info = ICONINFO::default();
    // SAFETY: `icon` is the shared cursor handle; `icon_info` a live out
    // value whose two bitmaps are deleted right below.
    let hotspot = unsafe { GetIconInfo(icon, &raw mut icon_info) }
        .ok()
        .map(|()| Point {
            x: i32::try_from(icon_info.xHotspot).unwrap_or(0),
            y: i32::try_from(icon_info.yHotspot).unwrap_or(0),
        });
    for bitmap in [icon_info.hbmMask, icon_info.hbmColor] {
        if !bitmap.is_invalid() {
            // SAFETY: GetIconInfo created these bitmaps for the caller.
            let _deleted = unsafe { DeleteObject(HGDIOBJ(bitmap.0)) };
        }
    }
    Some(Cursor {
        icon,
        position: Point {
            x: info.ptScreenPos.x,
            y: info.ptScreenPos.y,
        },
        hotspot: hotspot.unwrap_or(Point { x: 0, y: 0 }),
    })
}

/// Draw `area` into `dib` (sized to it), with the cursor as `mode` asks.
fn draw(area: Area, dib: &Dib, mode: CursorMode) -> Result<Option<CursorState>, CaptureError> {
    let screen = ScreenDc::get()?;
    let mem = MemDc::with(&screen, dib)?;
    let rect = area.rect();
    let (w, h) = (
        i32::try_from(rect.width).map_err(|e| gdi_err("BitBlt", e))?,
        i32::try_from(rect.height).map_err(|e| gdi_err("BitBlt", e))?,
    );
    match area {
        Area::Screen(_) => {
            // SAFETY: both DCs are live; the DIB holds `w` × `h` pixels.
            unsafe {
                BitBlt(
                    mem.dc,
                    0,
                    0,
                    w,
                    h,
                    Some(screen.0),
                    rect.x,
                    rect.y,
                    SRCCOPY | CAPTUREBLT,
                )
            }
            .map_err(|e| CaptureError::os(ID, "BitBlt", i64::from(e.code().0)))?;
        }
        Area::Window(hwnd, _) => {
            // SAFETY: the window handle was checked; the DC is live and its
            // DIB is the window rectangle's size.
            let printed = unsafe {
                PrintWindow(
                    listing::hwnd(hwnd),
                    mem.dc,
                    PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT),
                )
            };
            if !printed.as_bool() {
                return Err(if listing::window_exists(hwnd) {
                    gdi_err("PrintWindow", "the window could not be drawn")
                } else {
                    CaptureError::SourceLost
                });
            }
        }
    }
    let state = match mode {
        CursorMode::Hidden => None,
        CursorMode::Embedded | CursorMode::Metadata => cursor().and_then(|c| {
            let x = c.position.x - rect.x;
            let y = c.position.y - rect.y;
            if mode == CursorMode::Embedded {
                // SAFETY: the DC and the cursor handle are live; drawing
                // outside the DIB is clipped by GDI.
                let _drawn = unsafe {
                    DrawIconEx(
                        mem.dc,
                        x - c.hotspot.x,
                        y - c.hotspot.y,
                        c.icon,
                        0,
                        0,
                        0,
                        None,
                        DI_NORMAL,
                    )
                };
                None
            } else {
                Some(CursorState {
                    position: Point { x, y },
                    hotspot: c.hotspot,
                    visible: true,
                })
            }
        }),
    };
    drop(mem);
    // SAFETY: makes GDI finish drawing into the DIB before it is read.
    let _flushed = unsafe { GdiFlush() };
    Ok(state)
}

/// How a window may be drawn from this thread: `PrintWindow` waits for
/// the window's owner thread to handle `WM_PRINT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrintRoute {
    /// This thread owns the window: the message is handled inline.
    Direct,
    /// Another process's responsive window: drawn on a helper thread,
    /// bounded by [`PRINT_WAIT`].
    Helper,
    /// Another thread of this process owns the window: it may be the
    /// very thread waiting for this capture (a deadlock), so refused.
    OwnProcess,
    /// The owner is not answering messages (`IsHungAppWindow`).
    Hung,
}

const fn print_route(own_process: bool, own_thread: bool, hung: bool) -> PrintRoute {
    if own_process {
        if own_thread {
            PrintRoute::Direct
        } else {
            PrintRoute::OwnProcess
        }
    } else if hung {
        PrintRoute::Hung
    } else {
        PrintRoute::Helper
    }
}

/// The route to draw `hwnd` from the calling thread.
fn route_of(hwnd: isize) -> PrintRoute {
    let window = listing::hwnd(hwnd);
    let mut pid = 0_u32;
    // SAFETY: `pid` is a live out value; a stale handle answers 0.
    let thread = unsafe { GetWindowThreadProcessId(window, Some(&raw mut pid)) };
    // SAFETY: no arguments.
    let own_pid = unsafe { GetCurrentProcessId() };
    // SAFETY: no arguments.
    let own_thread = unsafe { GetCurrentThreadId() };
    // SAFETY: any handle value is accepted.
    let hung = unsafe { IsHungAppWindow(window) }.as_bool();
    print_route(pid == own_pid, thread == own_thread, hung)
}

fn own_window_refused() -> CaptureError {
    gdi_err(
        "PrintWindow",
        "a window of this process can only be drawn by GDI from its own thread",
    )
}

/// What a bounded grab gave.
enum Drawn {
    Done(Option<CursorState>),
    /// The window did not draw in time (or is hung): no frame.
    Missed,
}

/// [`draw`], with `PrintWindow` of another process's window on a helper
/// thread bounded by [`PRINT_WAIT`]. `in_flight` is set while a helper
/// runs; a helper that overran is never joined, and no new one starts
/// while it is still stuck. The DIB a stuck helper draws into is its own
/// `Arc`, never handed out again by the caller.
fn draw_bounded(
    area: Area,
    dib: &Arc<Dib>,
    mode: CursorMode,
    in_flight: &Arc<AtomicBool>,
) -> Result<Drawn, CaptureError> {
    let Area::Window(hwnd, _) = area else {
        return draw(area, dib, mode).map(Drawn::Done);
    };
    match route_of(hwnd) {
        PrintRoute::Direct => draw(area, dib, mode).map(Drawn::Done),
        PrintRoute::OwnProcess => Err(own_window_refused()),
        PrintRoute::Hung => Ok(Drawn::Missed),
        PrintRoute::Helper => {
            if in_flight.swap(true, Ordering::SeqCst) {
                return Ok(Drawn::Missed);
            }
            let (tx, rx) = flume::bounded(1);
            let target = Arc::clone(dib);
            let flag = Arc::clone(in_flight);
            let spawned = std::thread::Builder::new()
                .name("istmo-screen-capture-gdi-print".to_owned())
                .spawn(move || {
                    let _pixels = PhysicalPixels::enter();
                    let _sent = tx.send(draw(area, &target, mode));
                    flag.store(false, Ordering::SeqCst);
                });
            if let Err(err) = spawned {
                in_flight.store(false, Ordering::SeqCst);
                return Err(gdi_err("spawn print thread", err));
            }
            rx.recv_timeout(PRINT_WAIT)
                .map_or(Ok(Drawn::Missed), |drawn| drawn.map(Drawn::Done))
        }
    }
}

/// The visible frame of a window grab (without the invisible resize
/// borders), relative to the window rectangle.
fn content_rect(area: Area, size: Size) -> Rect {
    match area {
        Area::Screen(_) => Rect::from_size(size),
        Area::Window(hwnd, rect) => listing::frame_bounds(hwnd)
            .and_then(|frame| frame.intersect(rect))
            .map_or_else(
                || Rect::from_size(size),
                |frame| frame.relative_to(rect.x, rect.y),
            ),
    }
}

fn offers() -> Vec<Offer> {
    vec![Offer {
        format: FORMAT,
        memory: BufferMemory::Cpu,
        modifiers: Vec::new(),
        copies: vec![CopyStage::ShmTransport],
    }]
}

fn choose_tier(formats: &FormatRequest) -> Result<u8, CaptureError> {
    select::negotiate(formats, &offers(), 0)
        .map(|chosen| chosen.tier)
        .ok_or_else(|| CaptureError::unsatisfiable(Some(ID), Requirement::Formats))
}

fn caps() -> BackendCaps {
    let mut caps = BackendCaps::new(ID);
    caps.source_kinds = vec![SourceKind::Monitor, SourceKind::Window];
    caps.listing = SourceListing::Enumerable;
    caps.consent = ConsentModel::NoneNeeded;
    caps.video = vec![FormatCaps {
        format: FORMAT,
        memory: BufferMemory::Cpu,
        modifiers: Vec::new(),
        copies: vec![CopyStage::ShmTransport],
    }];
    caps.scaling = Scaling::Unsupported;
    caps.region = RegionCrop::InMeta;
    caps.composite = CompositeCaps {
        screenshot: CompositeSupport::CpuOnly,
        streaming: CompositeSupport::Unsupported,
    };
    caps.cursor = CURSOR_MODES.to_vec();
    caps.damage = DamageSupport::Unsupported;
    caps.orientation = OrientationSupport::AlreadyApplied;
    caps.pool = PoolCaps {
        exhaustion: vec![
            Exhaustion::Stall,
            Exhaustion::DropNewest,
            Exhaustion::CopyToCpu,
        ],
        ..POOL
    };
    caps.copies = vec![CopyStage::ShmTransport, CopyStage::ExhaustionReadback];
    caps.protected_content = ProtectedContent::MaskedUndetectable;
    caps.window = WindowCaptureCaps {
        occluded: true,
        minimised: false,
        child_popups: false,
        border: false,
    };
    caps.provenance = CapsProvenance {
        source_kinds: Provenance::Table,
        listing: Provenance::Probe,
        video: Provenance::Table,
        cursor: Provenance::Table,
        damage: Provenance::Table,
        colour: Provenance::Table,
        audio: Provenance::Table,
        producer: Provenance::Table,
        sync: Provenance::Table,
        persistence: Provenance::Table,
    };
    caps
}

fn listing_of() -> SourceList {
    let mut sources: Vec<Source> = listing::monitors(ID, false)
        .into_iter()
        .map(|m| m.source)
        .collect();
    sources.extend(listing::windows(ID, &SourceCapture::ById));
    SourceList {
        sources,
        listing: SourceListing::Enumerable,
    }
}

fn facts(size: Size, stride: u32, cursor: Option<CursorState>, protected: bool) -> FrameFacts {
    FrameFacts {
        size,
        format: FORMAT,
        memory: BufferMemory::Cpu,
        stride,
        pts_ns: os::qpc_now_ns(),
        damage: Damage::Full,
        cursor,
        orientation: crate::types::Orientation::UPRIGHT,
        producer: None,
        protected,
    }
}

fn stream_facts(
    size: Size,
    tier: u8,
    pool_depth: u8,
    exhaustion: Exhaustion,
    cursor: CursorMode,
) -> StreamFacts {
    StreamFacts {
        backend: ID,
        size,
        format: FORMAT,
        memory: BufferMemory::Cpu,
        tier,
        copies: vec![CopyStage::ShmTransport],
        pool_depth,
        exhaustion,
        orientation: OrientationSupport::AlreadyApplied,
        damage: DamageSupport::Unsupported,
        cursor,
        producer: None,
        border: BorderState::NotApplicable,
    }
}

#[derive(Debug)]
struct GdiBackend;

impl Backend for GdiBackend {
    fn id(&self) -> BackendId {
        ID
    }

    fn availability(&self) -> Availability {
        match ScreenDc::get() {
            Ok(_) => Availability::Ready,
            Err(err) => Availability::NotOnThisSession(err.to_string()),
        }
    }

    fn joins_auto(&self) -> bool {
        // Not before its checklist passed on a real Windows session.
        false
    }

    fn caps(&self) -> Result<BackendCaps, CaptureError> {
        Ok(caps())
    }

    fn sources(&self) -> Result<SourceList, CaptureError> {
        Ok(listing_of())
    }

    fn watch(&self, feed: &mut SourceFeed) -> Result<(), CaptureError> {
        super::watch::run(ID, feed, listing_of)
    }

    fn resolve_special(&self, selector: &SourceSelector) -> Result<Source, CaptureError> {
        match selector {
            SourceSelector::Focused => listing::focused(ID),
            SourceSelector::UnderCursor => listing::under_cursor(ID, false),
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
        let _pixels = PhysicalPixels::enter();
        let tier = choose_tier(&plan.formats)?;
        let mode = select::cursor_mode(plan.cursor, &CURSOR_MODES);
        if plan.cursor.strength == Strength::Require && mode != plan.cursor.mode {
            return Err(CaptureError::unsatisfiable(Some(ID), Requirement::Cursor));
        }
        let mut frames = Vec::with_capacity(sources.len());
        let mut first = None;
        for source in sources {
            let area = area_of(source)?;
            let dib = Arc::new(Dib::new(area.rect().size())?);
            let in_flight = Arc::new(AtomicBool::new(false));
            let Drawn::Done(cursor) = draw_bounded(area, &dib, mode, &in_flight)? else {
                return Err(gdi_err(
                    "PrintWindow",
                    "the window did not draw in time (busy or not responding)",
                ));
            };
            let size = dib.size;
            let mut meta = frame_meta(
                source,
                facts(size, dib.stride(), cursor, source.flags.protected),
            );
            meta.tier = tier;
            meta.content_rect = content_rect(area, size);
            if first.is_none() {
                let mut stream =
                    negotiated(source, stream_facts(size, tier, 1, Exhaustion::Stall, mode));
                stream.content_rect = meta.content_rect;
                first = Some(stream);
            }
            frames.push(Captured {
                meta,
                buffer: istmo_core::NativeBuffer::from_bytes(ArcDib(dib)),
                source: source.clone(),
            });
        }
        let negotiated = first.ok_or_else(|| gdi_err("screenshot", "no source"))?;
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
        let tier = choose_tier(&plan.formats)?;
        let mode = select::cursor_mode(plan.cursor, &CURSOR_MODES);
        let area = area_of(&plan.source)?;
        // The capture thread never owns a window.
        if let Area::Window(hwnd, _) = area
            && route_of(hwnd) == PrintRoute::OwnProcess
        {
            return Err(own_window_refused());
        }
        let size = area.rect().size();
        let depth = plan.pool_depth.max(2);
        let mut negotiated = negotiated(
            &plan.source,
            stream_facts(size, tier, depth, plan.exhaustion, mode),
        );
        negotiated.content_rect = content_rect(area, size);
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            cursor: Mutex::new(mode),
            negotiated: Mutex::new(negotiated.clone()),
        });
        let worker = Worker {
            source: plan.source,
            out,
            shared: Arc::clone(&shared),
            pool: PoolAccounting::new(usize::from(depth)),
            slots: Vec::new(),
            pool_depth: depth,
            exhaustion: plan.exhaustion,
            tier,
            area,
            printing: Arc::new(AtomicBool::new(false)),
            missed: 0,
        };
        let (done_tx, done) = flume::bounded(1);
        let thread = std::thread::Builder::new()
            .name("istmo-screen-capture-gdi".to_owned())
            .spawn(move || {
                worker.run();
                let _finished = done_tx.send(());
            })
            .map_err(|e| gdi_err("spawn capture thread", e))?;
        Ok((
            Box::new(Session {
                shared,
                done,
                thread: Some(thread),
            }),
            negotiated,
        ))
    }
}

/// What the capture thread shares with the session handle.
#[derive(Debug)]
struct Shared {
    stop: AtomicBool,
    cursor: Mutex<CursorMode>,
    negotiated: Mutex<NegotiatedStream>,
}

struct Session {
    shared: Arc<Shared>,
    done: flume::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl BackendSession for Session {
    fn update(&mut self, update: &SessionUpdate) -> Result<NegotiatedStream, CaptureError> {
        let mut negotiated = self
            .shared
            .negotiated
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match update {
            SessionUpdate::Cursor(request) => {
                let mode = select::cursor_mode(*request, &CURSOR_MODES);
                if request.strength == Strength::Require && mode != request.mode {
                    return Err(CaptureError::unsatisfiable(Some(ID), Requirement::Cursor));
                }
                *self
                    .shared
                    .cursor
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = mode;
                negotiated.cursor = mode;
            }
            SessionUpdate::Formats(formats) => {
                negotiated.tier = choose_tier(formats)?;
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
        Ok(negotiated.clone())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if self.done.recv_timeout(JOIN_WAIT).is_ok() {
            if let Some(thread) = self.thread.take() {
                let _joined = thread.join();
            }
        } else {
            tracing::warn!("gdi capture thread did not stop in time; detached");
        }
    }
}

struct Worker {
    source: Source,
    out: FrameOut,
    shared: Arc<Shared>,
    pool: PoolAccounting,
    /// One DIB per slot, all of the current size.
    slots: Vec<Option<Arc<Dib>>>,
    pool_depth: u8,
    exhaustion: Exhaustion,
    tier: u8,
    area: Area,
    /// A `PrintWindow` helper is running.
    printing: Arc<AtomicBool>,
    /// Frames missed in a row (window hung or slow).
    missed: u32,
}

impl Worker {
    fn period(&self) -> Duration {
        period(self.out.fps(), self.source.refresh_mhz)
    }

    /// Re-read where the source is: `false` when it is minimised.
    fn check_source(&mut self) -> Result<bool, CaptureError> {
        let area = area_of(&self.source)?;
        if let Area::Window(hwnd, _) = area
            && listing::is_minimised(hwnd)
        {
            return Ok(false);
        }
        if area.rect().size() != self.area.rect().size() {
            let size = area.rect().size();
            let generation = self.pool.reallocate(usize::from(self.pool_depth));
            tracing::debug!(generation, "gdi source resized");
            self.slots.clear();
            self.out.event(CaptureEvent::SourceResized(size));
            let mut negotiated = self
                .shared
                .negotiated
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            negotiated.size = size;
            negotiated.content_rect = content_rect(area, size);
            self.out
                .event(CaptureEvent::Renegotiated(negotiated.clone()));
        }
        self.area = area;
        Ok(true)
    }

    fn run(mut self) {
        let _pixels = PhysicalPixels::enter();
        let mut visible = true;
        let mut last_check: Option<Instant> = None;
        let mut last_capture: Option<Instant> = None;
        let mut skipped: u32 = 0;
        loop {
            if self.shared.stop.load(Ordering::SeqCst) || self.out.is_stopped() {
                break;
            }
            if last_check.is_none_or(|at| at.elapsed() >= SOURCE_CHECK) {
                last_check = Some(Instant::now());
                match self.check_source() {
                    Ok(now) if now != visible => {
                        visible = now;
                        self.out.event(if now {
                            CaptureEvent::SourceVisible
                        } else {
                            CaptureEvent::SourceHidden
                        });
                    }
                    Ok(_) => {}
                    Err(err) => {
                        self.out.fail(err);
                        break;
                    }
                }
            }
            if !self.out.wants_frames() {
                self.out.wait_while_paused(IDLE_WAIT);
                continue;
            }
            if !visible {
                std::thread::sleep(SOURCE_CHECK);
                continue;
            }
            let until_due = last_capture.map_or(Duration::ZERO, |at| {
                self.period().saturating_sub(at.elapsed())
            });
            if !until_due.is_zero() {
                std::thread::sleep(until_due.min(IDLE_WAIT));
                continue;
            }
            last_capture = Some(Instant::now());
            match self.capture_one(&mut skipped) {
                Ok(Delivery::Closed) => break,
                Ok(_) => {}
                Err(err) => {
                    self.out.fail(err);
                    break;
                }
            }
        }
    }

    fn slot_dib(&mut self, slot: u32) -> Result<Arc<Dib>, CaptureError> {
        let size = self.area.rect().size();
        let index = usize::try_from(slot).unwrap_or(usize::MAX);
        if let Some(Some(dib)) = self.slots.get(index)
            && dib.size == size
        {
            return Ok(Arc::clone(dib));
        }
        let dib = Arc::new(Dib::new(size)?);
        if self.slots.len() <= index {
            self.slots.resize_with(index + 1, || None);
        }
        self.slots[index] = Some(Arc::clone(&dib));
        Ok(dib)
    }

    /// Drop the DIB of `slot`, so the next frame there gets a new one.
    fn forget_slot(&mut self, slot: u32) {
        let index = usize::try_from(slot).unwrap_or(usize::MAX);
        if let Some(dib) = self.slots.get_mut(index) {
            *dib = None;
        }
    }

    fn capture_one(&mut self, skipped: &mut u32) -> Result<Delivery, CaptureError> {
        let slot = (0..u32::from(self.pool_depth))
            .find(|s| !self.pool.is_held(*s))
            .unwrap_or(0);
        let lease = match self.pool.hold(slot) {
            Ok(lease) => Some(lease),
            Err(err @ HoldError::SlotOutOfRange { .. }) => return Err(gdi_err("pool", err)),
            Err(HoldError::Exhausted(exhausted)) => {
                self.out
                    .exhausted(exhausted, self.pool_depth, self.exhaustion);
                *skipped = skipped.saturating_add(1);
                match self.exhaustion {
                    Exhaustion::Stall => {
                        let _woken = self.pool.released().recv_timeout(self.period());
                        return Ok(Delivery::Dropped);
                    }
                    Exhaustion::DropNewest => return Ok(Delivery::Dropped),
                    Exhaustion::CopyToCpu => None,
                }
            }
        };
        let mode = *self
            .shared
            .cursor
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // A lease-free frame (CopyToCpu) gets a DIB of its own, pinning no
        // slot.
        let dib = match &lease {
            Some(lease) => self.slot_dib(lease.identity().slot)?,
            None => Arc::new(Dib::new(self.area.rect().size())?),
        };
        let cursor = match draw_bounded(self.area, &dib, mode, &self.printing)? {
            Drawn::Done(cursor) => {
                self.missed = 0;
                cursor
            }
            Drawn::Missed => {
                // A helper may still draw into this DIB: never reuse it.
                if let Some(lease) = &lease {
                    self.forget_slot(lease.identity().slot);
                }
                self.missed += 1;
                *skipped = skipped.saturating_add(1);
                if self.missed >= PRINT_MISSES {
                    return Err(gdi_err(
                        "PrintWindow",
                        "the window stopped drawing (busy or not responding)",
                    ));
                }
                return Ok(Delivery::Dropped);
            }
        };
        let mut meta = frame_meta(
            &self.source,
            facts(dib.size, dib.stride(), cursor, self.source.flags.protected),
        );
        meta.tier = self.tier;
        meta.content_rect = content_rect(self.area, dib.size);
        meta.skipped_before = std::mem::take(skipped);
        let buffer = istmo_core::NativeBuffer::from_bytes(ArcDib(dib));
        let delivery = if let Some(lease) = lease {
            meta.buffer = lease.identity();
            let identity = lease.identity().into();
            self.out.deliver(
                meta,
                buffer
                    .with_identity(identity)
                    .with_release(move || drop(lease)),
            )
        } else {
            meta.copied = Some(CopyStage::ExhaustionReadback);
            self.out.deliver(meta, buffer)
        };
        Ok(delivery)
    }
}

/// A pooled DIB as frame bytes.
#[derive(Debug)]
struct ArcDib(Arc<Dib>);

impl AsRef<[u8]> for ArcDib {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref().as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_drawn_only_where_printwindow_cannot_block_forever() {
        assert_eq!(print_route(true, true, false), PrintRoute::Direct);
        assert_eq!(print_route(true, false, false), PrintRoute::OwnProcess);
        assert_eq!(print_route(true, false, true), PrintRoute::OwnProcess);
        assert_eq!(print_route(false, false, false), PrintRoute::Helper);
        assert_eq!(print_route(false, false, true), PrintRoute::Hung);
    }

    #[test]
    fn a_screenshot_of_the_primary_monitor_has_pixels() {
        let monitors = listing::monitors(ID, false);
        let Some(monitor) = monitors.first() else {
            eprintln!("no monitor: skipped");
            return;
        };
        let dib = Dib::new(monitor.rect.size()).expect("dib");
        draw(Area::Screen(monitor.rect), &dib, CursorMode::Hidden).expect("bitblt");
        assert_eq!(dib.as_ref().len(), dib.len());
    }
}
