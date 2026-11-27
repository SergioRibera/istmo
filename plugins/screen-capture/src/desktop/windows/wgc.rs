//! Windows.Graphics.Capture backend (`windows-wgc`): monitors and
//! windows through `IGraphicsCaptureItemInterop` (Windows 10 1903+), a
//! free-threaded `Direct3D11CaptureFramePool` on a D3D11 device of the
//! adapter the caller prefers.
//!
//! Three ways out, chosen by the format tiers:
//!
//! - **Zero-copy** (`D3d11Texture`, no copy): the pool's own texture,
//!   held (the `Direct3D11CaptureFrame` stays open) until the consumer
//!   drops the frame. The texture is the plugin device's and not
//!   shareable (`D3d11Frame::shared_handle` is `None`); "never the last
//!   free buffer" applies to the pool.
//! - **Shareable** (`D3d11Texture`, `CopyStage::CrossDevice`): asked for
//!   with `GpuPreference::Same(Luid)`: the pool runs on that adapter and
//!   each frame is copied into a plugin-owned NT-handle texture the
//!   consumer opens on its own device.
//! - **CPU** (`CopyStage::GpuReadback`): copied into a staging texture,
//!   mapped and copied out.
//!
//! Formats: BGRA8 and FP16 (scRGB, for HDR). Cursor: embedded, or hidden
//! where `IsCursorCaptureEnabled` exists (2004+); never metadata.
//! Damage: `Direct3D11CaptureFrame.DirtyRegions` where `DirtyRegionMode`
//! exists (24H2), otherwise every frame is full. `MinUpdateInterval`
//! (24H2) throttles at the source. Minimised windows deliver no frames
//! (`SourceHidden`); `WDA_EXCLUDEFROMCAPTURE` and DRM content come out
//! black, undetectably. The OS draws a yellow border unless borderless
//! capture was granted (`WindowsOptions::borderless`, build 20348+).

#![allow(
    unsafe_code,
    reason = "WinRT / Direct3D interop: IGraphicsCaptureItemInterop, IDirect3DDxgiInterfaceAccess"
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use istmo_core::NativeBuffer;
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureDirtyRegionMode,
    GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R16G16B16A16_FLOAT,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::Interface as _;

use crate::d3d11::{D3d11Frame, SharedHandle};
use crate::policy::CapturePolicy;
use crate::pool::{HoldError, PoolAccounting};
use crate::select::{self, Chosen, Offer};
use crate::types::{
    AccessScope, AccessState, AccessStatus, Availability, BackendCaps, BackendId, BorderState,
    Borderless, BufferMemory, CapsProvenance, CaptureError, CaptureEvent, CompositeCaps,
    CompositeSupport, ConsentModel, CopyStage, CursorMode, Damage, DamageSupport, Exhaustion,
    FormatCaps, FormatRequest, FrameMeta, FrameRate, GpuIdentity, GpuPreference, HdrPolicy,
    NegotiatedStream, OrientationSupport, Persistence, PixelFormat, PoolCaps, ProtectedContent,
    Provenance, Rect, RegionCrop, Requirement, Scaling, SessionUpdate, Size, Source, SourceCapture,
    SourceKind, SourceList, SourceListing, SourceSelector, Strength, WindowCaptureCaps,
    WindowsOptions,
};

use super::super::backend::{
    Backend, BackendSession, Captured, ScreenshotPlan, Shot, StreamPlan, Target,
};
use super::super::output::{Delivery, FrameOut};
use super::super::watch::SourceFeed;
use super::access;
use super::common::{FrameFacts, StreamFacts, damage_of, frame_meta, negotiated};
use super::d3d::{self, Gpu, Readback};
use super::listing::{self, Target as WinTarget};
use super::os;

const ID: BackendId = BackendId::WindowsWgc;
const SESSION_CLASS: &str = "Windows.Graphics.Capture.GraphicsCaptureSession";
const FRAME_CLASS: &str = "Windows.Graphics.Capture.Direct3D11CaptureFrame";

pub(crate) fn backend() -> Arc<dyn Backend> {
    Arc::new(WgcBackend)
}

/// Frame-pool limits (`numberOfBuffers`).
const POOL: PoolCaps = PoolCaps {
    min_depth: 2,
    max_depth: 8,
    default_depth: 3,
    exhaustion: Vec::new(),
};
/// Longest wait for a frame, so the stop flag is seen.
const FRAME_WAIT: Duration = Duration::from_millis(100);
/// How long a session may take to start.
const START_WAIT: Duration = Duration::from_secs(5);
/// How long a screenshot waits for its frame.
const SCREENSHOT_WAIT: Duration = Duration::from_secs(2);
/// How long the GPU may take to finish a copy.
const COPY_WAIT: Duration = Duration::from_millis(100);
/// How often a window's minimised state is read.
const VISIBILITY_CHECK: Duration = Duration::from_millis(250);
/// How long dropping a session waits for its thread.
const JOIN_WAIT: Duration = Duration::from_secs(2);

fn hr(operation: &str, err: &windows_core::Error) -> CaptureError {
    CaptureError::os(ID, operation, i64::from(err.code().0))
}

fn wgc_err(step: &str, message: impl std::fmt::Display) -> CaptureError {
    CaptureError::protocol(ID, step, message)
}

/// The optional parts of the API this Windows has (probed).
#[derive(Debug, Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one probe per optional property"
)]
struct Features {
    /// `IsCursorCaptureEnabled` (2004).
    cursor_toggle: bool,
    /// `IsBorderRequired` (build 20348).
    border: bool,
    /// `DirtyRegionMode` + frame `DirtyRegions` (24H2).
    dirty: bool,
    /// `MinUpdateInterval` (24H2).
    min_interval: bool,
    /// `IncludeSecondaryWindows` (child popups of a captured window).
    secondary: bool,
}

fn features() -> Features {
    Features {
        cursor_toggle: os::has_property(SESSION_CLASS, "IsCursorCaptureEnabled"),
        border: access::borderless_supported(),
        dirty: os::has_property(SESSION_CLASS, "DirtyRegionMode")
            && os::has_property(FRAME_CLASS, "DirtyRegions"),
        min_interval: os::has_property(SESSION_CLASS, "MinUpdateInterval"),
        secondary: os::has_property(SESSION_CLASS, "IncludeSecondaryWindows"),
    }
}

fn cursor_modes(features: Features) -> Vec<CursorMode> {
    if features.cursor_toggle {
        vec![CursorMode::Hidden, CursorMode::Embedded]
    } else {
        // Before 2004 the cursor is always in the image.
        vec![CursorMode::Embedded]
    }
}

const FORMATS: [PixelFormat; 2] = [PixelFormat::Bgra8, PixelFormat::Rgba16f];

const fn directx_format(format: PixelFormat) -> DirectXPixelFormat {
    match format {
        PixelFormat::Rgba16f => DirectXPixelFormat::R16G16B16A16Float,
        _ => DirectXPixelFormat::B8G8R8A8UIntNormalized,
    }
}

/// What WGC can deliver. `shareable`: the consumer opens frames on its
/// own device (`GpuPreference::Same`), so the copy-free pool textures,
/// which it cannot open, are not offered.
fn offers(formats: &[PixelFormat], shareable: bool) -> Vec<Offer> {
    formats
        .iter()
        .flat_map(|format| {
            let texture = |copies: Vec<CopyStage>| Offer {
                format: *format,
                memory: BufferMemory::D3d11Texture,
                modifiers: Vec::new(),
                copies,
            };
            let mut offers = Vec::new();
            if !shareable {
                offers.push(texture(Vec::new()));
            }
            offers.push(texture(vec![CopyStage::CrossDevice]));
            offers.push(Offer {
                format: *format,
                memory: BufferMemory::Cpu,
                modifiers: Vec::new(),
                copies: vec![CopyStage::GpuReadback],
            });
            offers
        })
        .collect()
}

fn caps() -> BackendCaps {
    let features = features();
    let mut caps = BackendCaps::new(ID);
    caps.source_kinds = vec![SourceKind::Monitor, SourceKind::Window];
    caps.listing = SourceListing::Enumerable;
    caps.consent = ConsentModel::NoneNeeded;
    caps.video = offers(&FORMATS, false)
        .into_iter()
        .map(|o| FormatCaps {
            format: o.format,
            memory: o.memory,
            modifiers: Vec::new(),
            copies: o.copies,
        })
        .collect();
    caps.scaling = Scaling::Unsupported;
    caps.region = RegionCrop::InMeta;
    caps.composite = CompositeCaps {
        screenshot: CompositeSupport::CpuOnly,
        streaming: CompositeSupport::Unsupported,
    };
    caps.cursor = cursor_modes(features);
    caps.damage = if features.dirty {
        DamageSupport::Rects
    } else {
        DamageSupport::Unsupported
    };
    caps.orientation = OrientationSupport::AlreadyApplied;
    caps.colour.hdr = true;
    caps.colour.colours.push(super::common::SCRGB);
    caps.producer = d3d::adapters(ID)
        .ok()
        .and_then(|a| a.into_iter().find(|a| !a.software))
        .map(|a| GpuIdentity::Luid(a.luid));
    caps.persistence = if features.border {
        Persistence::SystemSetting
    } else {
        Persistence::Unsupported
    };
    caps.pool = PoolCaps {
        exhaustion: vec![
            Exhaustion::Stall,
            Exhaustion::DropNewest,
            Exhaustion::CopyToCpu,
        ],
        ..POOL
    };
    caps.copies = vec![
        CopyStage::CrossDevice,
        CopyStage::GpuReadback,
        CopyStage::ExhaustionReadback,
    ];
    caps.protected_content = ProtectedContent::MaskedUndetectable;
    caps.window = WindowCaptureCaps {
        occluded: true,
        minimised: false,
        child_popups: features.secondary,
        border: true,
    };
    caps.min_interval_us = None;
    caps.provenance = CapsProvenance {
        source_kinds: Provenance::Table,
        listing: Provenance::Probe,
        video: Provenance::Table,
        cursor: Provenance::Probe,
        damage: Provenance::Probe,
        colour: Provenance::Table,
        audio: Provenance::Table,
        producer: Provenance::Probe,
        sync: Provenance::Table,
        persistence: Provenance::Probe,
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

/// The capture item of `source`, and the window handle for windows.
fn item_for(source: &Source) -> Result<(GraphicsCaptureItem, Option<isize>), CaptureError> {
    let interop = windows_core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .map_err(|e| hr("IGraphicsCaptureItemInterop", &e))?;
    match listing::target_of(ID, source)? {
        WinTarget::Monitor(device) => {
            let monitor = listing::monitor(ID, &device, false).ok_or(CaptureError::SourceLost)?;
            // SAFETY: `monitor` names a monitor enumerated just now.
            let item = unsafe { interop.CreateForMonitor::<GraphicsCaptureItem>(monitor.handle()) }
                .map_err(|e| hr("CreateForMonitor", &e))?;
            Ok((item, None))
        }
        WinTarget::Window(hwnd) => {
            if !listing::window_exists(hwnd) {
                return Err(CaptureError::SourceLost);
            }
            // SAFETY: `hwnd` names an existing window.
            let item =
                unsafe { interop.CreateForWindow::<GraphicsCaptureItem>(listing::hwnd(hwnd)) }
                    .map_err(|e| hr("CreateForWindow", &e))?;
            Ok((item, Some(hwnd)))
        }
    }
}

/// The WinRT view of a D3D11 device, for the frame pool.
fn winrt_device(gpu: &Gpu) -> Result<IDirect3DDevice, CaptureError> {
    let dxgi = gpu
        .device
        .cast::<IDXGIDevice>()
        .map_err(|e| hr("IDXGIDevice", &e))?;
    // SAFETY: `dxgi` is the live device's DXGI interface.
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
        .map_err(|e| hr("CreateDirect3D11DeviceFromDXGIDevice", &e))?;
    inspectable
        .cast::<IDirect3DDevice>()
        .map_err(|e| hr("IDirect3DDevice", &e))
}

/// The D3D11 texture behind a captured frame.
fn texture_of(frame: &Direct3D11CaptureFrame) -> Result<ID3D11Texture2D, CaptureError> {
    let surface = frame
        .Surface()
        .map_err(|e| hr("Direct3D11CaptureFrame.Surface", &e))?;
    let access = surface
        .cast::<IDirect3DDxgiInterfaceAccess>()
        .map_err(|e| hr("IDirect3DDxgiInterfaceAccess", &e))?;
    // SAFETY: the surface of a frame of a D3D11 frame pool is a D3D11
    // texture; the returned reference is owned.
    unsafe { access.GetInterface::<ID3D11Texture2D>() }.map_err(|e| hr("GetInterface", &e))
}

const fn size_of(size: SizeInt32) -> Size {
    Size::new(size.Width.unsigned_abs(), size.Height.unsigned_abs())
}

fn size_int(size: Size) -> SizeInt32 {
    SizeInt32 {
        Width: i32::try_from(size.width).unwrap_or(i32::MAX),
        Height: i32::try_from(size.height).unwrap_or(i32::MAX),
    }
}

/// Decide the border: `Ok(true)` to ask for none, with the state the
/// session then reports.
fn borderless(
    options: WindowsOptions,
    allow: bool,
    features: Features,
) -> Result<(bool, BorderState), CaptureError> {
    let shown = Ok((false, BorderState::Shown));
    let require = options.borderless == Borderless::Require;
    match options.borderless {
        Borderless::Off => return shown,
        Borderless::Prefer | Borderless::Require => {}
    }
    if !allow {
        return if require {
            Err(CaptureError::PolicyDenied(
                "allow_borderless is false".to_owned(),
            ))
        } else {
            shown
        };
    }
    if !features.border {
        return if require {
            Err(CaptureError::unsatisfiable(
                Some(ID),
                Requirement::Borderless,
            ))
        } else {
            shown
        };
    }
    let mut status = access::borderless_status();
    if status == AccessStatus::NotDetermined {
        status = access::request_borderless()?;
    }
    if status == AccessStatus::Granted {
        Ok((true, BorderState::Hidden))
    } else if require {
        Err(CaptureError::ConsentDenied)
    } else {
        shown
    }
}

/// How a session's frames leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Path {
    /// The pool's own texture, held until the consumer drops it.
    Texture,
    /// Copied into a shareable NT-handle texture.
    Shared,
    /// Read back into CPU memory.
    Cpu,
}

fn path_of(chosen: &Chosen) -> Path {
    if chosen.memory == BufferMemory::Cpu {
        Path::Cpu
    } else if chosen.copies.contains(&CopyStage::CrossDevice) {
        Path::Shared
    } else {
        Path::Texture
    }
}

/// The tier for `formats` and `hdr`: FP16 only when HDR is asked for.
fn choose(
    formats: &FormatRequest,
    hdr: HdrPolicy,
    shareable: bool,
) -> Result<Chosen, CaptureError> {
    let candidates: &[PixelFormat] = match hdr {
        HdrPolicy::Sdr => &[PixelFormat::Bgra8],
        HdrPolicy::PreferHdr => &[PixelFormat::Rgba16f, PixelFormat::Bgra8],
        HdrPolicy::RequireHdr => &[PixelFormat::Rgba16f],
    };
    // The caller's tier order decides; HDR preference picks inside it.
    let chosen = candidates
        .iter()
        .filter_map(|format| select::negotiate(formats, &offers(&[*format], shareable), 0))
        .min_by_key(|chosen| chosen.tier);
    chosen.ok_or_else(|| {
        if shareable && select::negotiate(formats, &offers(candidates, false), 0).is_some() {
            CaptureError::unsatisfiable(Some(ID), Requirement::Gpu)
        } else if hdr == HdrPolicy::RequireHdr {
            CaptureError::unsatisfiable(Some(ID), Requirement::Hdr)
        } else {
            CaptureError::unsatisfiable(Some(ID), Requirement::Formats)
        }
    })
}

/// What a capture is asked for.
#[derive(Debug, Clone, Copy)]
struct Setup {
    format: PixelFormat,
    buffers: u8,
    cursor: CursorMode,
    borderless: bool,
    dirty: bool,
    min_interval: Option<Duration>,
}

/// A started WGC capture; stops (session and pool closed) on drop.
struct Capture {
    device: IDirect3DDevice,
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    arrived: flume::Receiver<()>,
    closed: Arc<AtomicBool>,
    arrived_token: i64,
    closed_token: i64,
    pixel: DirectXPixelFormat,
    size: Size,
    buffers: u8,
    window: Option<isize>,
}

impl Capture {
    fn start(
        source: &Source,
        gpu: &Gpu,
        setup: Setup,
        features: Features,
    ) -> Result<Self, CaptureError> {
        let (item, window) = item_for(source)?;
        let device = winrt_device(gpu)?;
        let size = size_of(
            item.Size()
                .map_err(|e| hr("GraphicsCaptureItem.Size", &e))?,
        );
        if size.is_empty() {
            return Err(wgc_err(
                "GraphicsCaptureItem",
                "the source has no size (minimised?)",
            ));
        }
        let pixel = directx_format(setup.format);
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &device,
            pixel,
            i32::from(setup.buffers),
            size_int(size),
        )
        .map_err(|e| hr("Direct3D11CaptureFramePool.CreateFreeThreaded", &e))?;
        let (tx, arrived) = flume::bounded(1);
        let arrived_token = pool
            .FrameArrived(&TypedEventHandler::new(move |_, _| {
                // A full channel already says "a frame is waiting".
                let _woken = tx.try_send(());
                Ok(())
            }))
            .map_err(|e| hr("FrameArrived", &e))?;
        let closed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&closed);
        let closed_token = item
            .Closed(&TypedEventHandler::new(move |_, _| {
                flag.store(true, Ordering::SeqCst);
                Ok(())
            }))
            .map_err(|e| hr("GraphicsCaptureItem.Closed", &e))?;
        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|e| hr("CreateCaptureSession", &e))?;
        if features.cursor_toggle {
            session
                .SetIsCursorCaptureEnabled(setup.cursor == CursorMode::Embedded)
                .map_err(|e| hr("IsCursorCaptureEnabled", &e))?;
        }
        if features.border && setup.borderless {
            session
                .SetIsBorderRequired(false)
                .map_err(|e| hr("IsBorderRequired", &e))?;
        }
        if setup.dirty {
            session
                .SetDirtyRegionMode(GraphicsCaptureDirtyRegionMode::ReportOnly)
                .map_err(|e| hr("DirtyRegionMode", &e))?;
        }
        if let (true, Some(interval)) = (features.min_interval, setup.min_interval) {
            let ticks = i64::try_from(interval.as_nanos() / 100).unwrap_or(i64::MAX);
            session
                .SetMinUpdateInterval(TimeSpan { Duration: ticks })
                .map_err(|e| hr("MinUpdateInterval", &e))?;
        }
        if window.is_some() && features.secondary {
            // Child popups (menus, tooltips) of the captured window too.
            let _secondary = session.SetIncludeSecondaryWindows(true);
        }
        session.StartCapture().map_err(|e| hr("StartCapture", &e))?;
        Ok(Self {
            device,
            item,
            pool,
            session,
            arrived,
            closed,
            arrived_token,
            closed_token,
            pixel,
            size,
            buffers: setup.buffers,
            window,
        })
    }

    /// The newest frame (older queued ones are closed and counted in
    /// `skipped`), waiting at most `timeout`.
    fn next(&self, timeout: Duration, skipped: &mut u32) -> Option<Direct3D11CaptureFrame> {
        let mut newest: Option<Direct3D11CaptureFrame> = None;
        loop {
            if let Ok(frame) = self.pool.TryGetNextFrame() {
                if let Some(older) = newest.replace(frame) {
                    let _closed = older.Close();
                    *skipped = skipped.saturating_add(1);
                }
            } else {
                if newest.is_some() {
                    return newest;
                }
                if self.arrived.recv_timeout(timeout).is_err() {
                    return None;
                }
            }
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Re-create the pool's buffers at `size`.
    fn recreate(&mut self, size: Size) -> Result<(), CaptureError> {
        self.pool
            .Recreate(
                &self.device,
                self.pixel,
                i32::from(self.buffers),
                size_int(size),
            )
            .map_err(|e| hr("Direct3D11CaptureFramePool.Recreate", &e))?;
        self.size = size;
        Ok(())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _removed = self.pool.RemoveFrameArrived(self.arrived_token);
        let _removed = self.item.RemoveClosed(self.closed_token);
        let _closed = self.session.Close();
        let _closed = self.pool.Close();
    }
}

/// The capture time of a frame (`SystemRelativeTime` is QPC-based, in
/// 100 ns units), ns.
fn pts_of(frame: &Direct3D11CaptureFrame) -> u64 {
    frame
        .SystemRelativeTime()
        .ok()
        .and_then(|t| u64::try_from(t.Duration).ok())
        .map_or_else(os::qpc_now_ns, |ticks| ticks.saturating_mul(100))
}

fn dirty_of(frame: &Direct3D11CaptureFrame, size: Size) -> Damage {
    let Ok(regions) = frame.DirtyRegions() else {
        return Damage::Full;
    };
    let Ok(count) = regions.Size() else {
        return Damage::Full;
    };
    let rects = (0..count)
        .filter_map(|i| regions.GetAt(i).ok())
        .map(|r| Rect::new(r.X, r.Y, r.Width.unsigned_abs(), r.Height.unsigned_abs()));
    damage_of(rects, size)
}

fn gpu_for(preference: GpuPreference) -> Result<Gpu, CaptureError> {
    let adapter = d3d::adapter_for(ID, preference)?;
    Gpu::new(ID, adapter.as_ref())
}

#[expect(
    clippy::too_many_arguments,
    reason = "every fact of one negotiated stream, from one call site each"
)]
fn stream_facts(
    size: Size,
    chosen: &Chosen,
    pool_depth: u8,
    exhaustion: Exhaustion,
    cursor: CursorMode,
    dirty: bool,
    luid: u64,
    border: BorderState,
) -> StreamFacts {
    StreamFacts {
        backend: ID,
        size,
        format: chosen.format,
        memory: chosen.memory,
        tier: chosen.tier,
        copies: chosen.copies.clone(),
        pool_depth,
        exhaustion,
        orientation: OrientationSupport::AlreadyApplied,
        damage: if dirty {
            DamageSupport::Rects
        } else {
            DamageSupport::Unsupported
        },
        cursor,
        producer: Some(GpuIdentity::Luid(luid)),
        border,
    }
}

const fn min_interval(fps: FrameRate) -> Option<Duration> {
    match fps {
        FrameRate::Native => None,
        FrameRate::Max(n) | FrameRate::Exact(n) | FrameRate::OnDamage(n) => Some(
            Duration::from_nanos(1_000_000_000 / if n == 0 { 1 } else { n as u64 }),
        ),
    }
}

#[derive(Debug)]
struct WgcBackend;

impl Backend for WgcBackend {
    fn id(&self) -> BackendId {
        ID
    }

    fn availability(&self) -> Availability {
        if os::build() < os::BUILD_WGC_INTEROP {
            return Availability::OsTooOld(os::version());
        }
        match GraphicsCaptureSession::IsSupported() {
            Ok(true) => Availability::Ready,
            Ok(false) => Availability::NotOnThisSession(
                "Windows.Graphics.Capture is not supported here".to_owned(),
            ),
            Err(err) => {
                Availability::NotOnThisSession(format!("GraphicsCaptureSession.IsSupported: {err}"))
            }
        }
    }

    fn joins_auto(&self) -> bool {
        // Not before its checklist passed on a real Windows session.
        false
    }

    fn access(&self, scope: AccessScope) -> Option<AccessState> {
        access::current(scope)
    }

    fn request_access(
        &self,
        scope: AccessScope,
        policy: &CapturePolicy,
    ) -> Option<Result<AccessState, CaptureError>> {
        access::request(scope, policy)
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

    #[expect(
        clippy::too_many_lines,
        reason = "one capture per source, its readback or copy, and the negotiated stream"
    )]
    fn screenshot(&self, plan: &ScreenshotPlan) -> Result<Shot, CaptureError> {
        let Target::Sources { sources, .. } = &plan.target else {
            return Err(CaptureError::unsatisfiable(Some(ID), Requirement::Picker));
        };
        let features = features();
        let mode = select::cursor_mode(plan.cursor, &cursor_modes(features));
        if plan.cursor.strength == Strength::Require && mode != plan.cursor.mode {
            return Err(CaptureError::unsatisfiable(Some(ID), Requirement::Cursor));
        }
        let chosen = choose(&plan.formats, plan.hdr, false)?;
        let gpu = gpu_for(GpuPreference::Auto)?;
        let mut frames = Vec::with_capacity(sources.len());
        let mut first = None;
        for source in sources {
            let setup = Setup {
                format: chosen.format,
                buffers: 2,
                cursor: mode,
                borderless: false,
                dirty: false,
                min_interval: None,
            };
            let capture = Capture::start(source, &gpu, setup, features)?;
            let mut skipped = 0;
            let deadline = Instant::now() + SCREENSHOT_WAIT;
            let frame = loop {
                if capture.is_closed() {
                    return Err(CaptureError::SourceLost);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(wgc_err(
                        "screenshot",
                        "no frame in time (is the window minimised?)",
                    ));
                }
                if let Some(frame) = capture.next(remaining.min(FRAME_WAIT), &mut skipped) {
                    break frame;
                }
            };
            let texture = texture_of(&frame)?;
            let desc = d3d::texture_desc(&texture);
            let content = frame.ContentSize().map_or(capture.size, size_of);
            let size = Size::new(desc.Width, desc.Height);
            let facts = |memory, stride| FrameFacts {
                size,
                format: chosen.format,
                memory,
                stride,
                pts_ns: pts_of(&frame),
                damage: Damage::Full,
                cursor: None,
                orientation: crate::types::Orientation::UPRIGHT,
                producer: Some(GpuIdentity::Luid(gpu.luid)),
                protected: false,
            };
            let (mut meta, buffer) = if chosen.memory == BufferMemory::Cpu {
                let image = Readback::default().read(ID, &gpu, &texture)?;
                (
                    frame_meta(source, facts(BufferMemory::Cpu, image.stride)),
                    NativeBuffer::from_bytes(image.bytes),
                )
            } else {
                // A screenshot outlives its capture: copied out of the
                // frame pool into a texture of its own.
                let format = d3d::dxgi_format(chosen.format).unwrap_or(DXGI_FORMAT_B8G8R8A8_UNORM);
                let (copy, handle) = gpu.shared_texture(ID, size, format)?;
                gpu.copy(&copy, &texture);
                gpu.finish(ID, COPY_WAIT)?;
                let mut meta = frame_meta(source, facts(BufferMemory::D3d11Texture, 0));
                meta.copied = Some(CopyStage::LiveBufferSnapshot);
                let backing =
                    D3d11Frame::new(copy, gpu.device.clone(), Some(Arc::new(handle)), gpu.luid);
                (meta, NativeBuffer::from_platform(backing))
            };
            let _closed = frame.Close();
            drop(capture);
            meta.tier = chosen.tier;
            meta.content_rect = Rect::from_size(content)
                .intersect(Rect::from_size(size))
                .unwrap_or_else(|| Rect::from_size(size));
            if first.is_none() {
                let mut stream = negotiated(
                    source,
                    stream_facts(
                        size,
                        &chosen,
                        1,
                        Exhaustion::Stall,
                        mode,
                        false,
                        gpu.luid,
                        BorderState::Shown,
                    ),
                );
                stream.content_rect = meta.content_rect;
                first = Some(stream);
            }
            frames.push(Captured {
                meta,
                buffer,
                source: source.clone(),
            });
        }
        let negotiated = first.ok_or_else(|| wgc_err("screenshot", "no source"))?;
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
        let features = features();
        let shareable = matches!(plan.gpu, GpuPreference::Same(_));
        let chosen = choose(&plan.formats, plan.hdr, shareable)?;
        let path = path_of(&chosen);
        let mode = select::cursor_mode(plan.cursor, &cursor_modes(features));
        let (borderless, border) = borderless(plan.windows, plan.allow_borderless, features)?;
        let dirty = features.dirty && plan.damage != Strength::Ignore;
        let gpu = gpu_for(plan.gpu)?;
        let depth = plan.pool_depth.clamp(POOL.min_depth, POOL.max_depth);
        // Only the zero-copy path holds pool buffers; the others close
        // each frame at once.
        let buffers = if path == Path::Texture { depth } else { 2 };
        let setup = Setup {
            format: chosen.format,
            buffers,
            cursor: mode,
            borderless,
            dirty,
            min_interval: min_interval(plan.fps),
        };
        // WinRT capture objects are made and used on the capture thread
        // alone; it reports the started capture's size back.
        let (started_tx, started) = flume::bounded::<Result<Size, CaptureError>>(1);
        let (done_tx, done) = flume::bounded(1);
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            negotiated: Mutex::new(negotiated(
                &plan.source,
                stream_facts(
                    Size::new(0, 0),
                    &chosen,
                    depth,
                    plan.exhaustion,
                    mode,
                    dirty,
                    gpu.luid,
                    border,
                ),
            )),
        });
        let thread_shared = Arc::clone(&shared);
        let source = plan.source.clone();
        let exhaustion = plan.exhaustion;
        let thread = std::thread::Builder::new()
            .name("istmo-screen-capture-wgc".to_owned())
            .spawn(move || {
                let capture = match Capture::start(&source, &gpu, setup, features) {
                    Ok(capture) => capture,
                    Err(err) => {
                        let _told = started_tx.send(Err(err));
                        let _finished = done_tx.send(());
                        return;
                    }
                };
                if started_tx.send(Ok(capture.size)).is_err() {
                    let _finished = done_tx.send(());
                    return;
                }
                let worker = Worker {
                    capture: Some(capture),
                    gpu,
                    chosen,
                    path,
                    dirty,
                    source,
                    out,
                    shared: thread_shared,
                    pool: PoolAccounting::new(usize::from(depth)),
                    textures: Vec::new(),
                    slots: Vec::new(),
                    pool_depth: depth,
                    exhaustion,
                    readback: Readback::default(),
                };
                worker.run();
                let _finished = done_tx.send(());
            })
            .map_err(|e| wgc_err("spawn capture thread", e))?;
        let session = Session {
            shared: Arc::clone(&shared),
            done,
            thread: Some(thread),
        };
        let size = match started.recv_timeout(START_WAIT) {
            Ok(Ok(size)) => size,
            Ok(Err(err)) => return Err(err),
            Err(_) => return Err(wgc_err("StartCapture", "the capture did not start in time")),
        };
        let negotiated = {
            let mut negotiated = shared
                .negotiated
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            negotiated.size = size;
            negotiated.content_rect = Rect::from_size(size);
            negotiated.clone()
        };
        Ok((Box::new(session), negotiated))
    }
}

#[derive(Debug)]
struct Shared {
    stop: AtomicBool,
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
        // The cursor, the tier and the pool are fixed at `StartCapture`
        // here: a change is a new session.
        let _negotiated = self
            .shared
            .negotiated
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let _ = update;
        Err(CaptureError::unsatisfiable(
            Some(ID),
            Requirement::Operation,
        ))
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
            tracing::warn!("wgc capture thread did not stop in time; detached");
        }
    }
}

/// A shareable copy slot: the texture and its NT handle.
struct Slot {
    texture: ID3D11Texture2D,
    handle: Arc<SharedHandle>,
}

struct Worker {
    capture: Option<Capture>,
    gpu: Gpu,
    chosen: Chosen,
    path: Path,
    dirty: bool,
    source: Source,
    out: FrameOut,
    shared: Arc<Shared>,
    pool: PoolAccounting,
    /// The frame pool's textures seen so far (raw interface pointers):
    /// a texture's index is its slot.
    textures: Vec<usize>,
    /// Shareable copies (the `Shared` path), by slot.
    slots: Vec<Option<Slot>>,
    pool_depth: u8,
    exhaustion: Exhaustion,
    readback: Readback,
}

impl Worker {
    fn run(mut self) {
        let mut skipped: u32 = 0;
        let mut visible = true;
        let mut last_check: Option<Instant> = None;
        loop {
            if self.shared.stop.load(Ordering::SeqCst) || self.out.is_stopped() {
                break;
            }
            let Some(capture) = self.capture.as_ref() else {
                break;
            };
            if capture.is_closed() {
                self.out.fail(CaptureError::SourceLost);
                break;
            }
            if let Some(hwnd) = capture.window
                && last_check.is_none_or(|at| at.elapsed() >= VISIBILITY_CHECK)
            {
                last_check = Some(Instant::now());
                if !listing::window_exists(hwnd) {
                    self.out.fail(CaptureError::SourceLost);
                    break;
                }
                let now = !listing::is_minimised(hwnd);
                if now != visible {
                    visible = now;
                    self.out.event(if now {
                        CaptureEvent::SourceVisible
                    } else {
                        CaptureEvent::SourceHidden
                    });
                }
            }
            let Some(frame) = capture.next(FRAME_WAIT, &mut skipped) else {
                continue;
            };
            if !self.out.wants_frames() {
                // Paused: WGC frames are released at once.
                let _closed = frame.Close();
                self.out.wait_while_paused(FRAME_WAIT);
                continue;
            }
            match self.handle(frame, &mut skipped) {
                Ok(Delivery::Closed) => break,
                Ok(_) => {}
                Err(err) => {
                    self.out.fail(err);
                    break;
                }
            }
        }
        // Close the capture (and with it the border) before the thread
        // ends; frames still held stay valid until dropped.
        drop(self.capture.take());
    }

    /// The content size changed: new buffers, a new generation.
    fn resize(&mut self, size: Size) -> Result<(), CaptureError> {
        if let Some(capture) = self.capture.as_mut() {
            capture.recreate(size)?;
        }
        let generation = self.pool.reallocate(usize::from(self.pool_depth));
        tracing::debug!(generation, "wgc source resized");
        self.textures.clear();
        self.slots.clear();
        self.readback.reset();
        self.out.event(CaptureEvent::SourceResized(size));
        let renegotiated = {
            let mut negotiated = self
                .shared
                .negotiated
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            negotiated.size = size;
            negotiated.content_rect = Rect::from_size(size);
            negotiated.fps = self.out.fps();
            negotiated.clone()
        };
        self.out.event(CaptureEvent::Renegotiated(renegotiated));
        Ok(())
    }

    /// The slot of a pool texture, by identity.
    fn texture_slot(&mut self, texture: &ID3D11Texture2D) -> u32 {
        let raw = texture.as_raw() as usize;
        let index = if let Some(index) = self.textures.iter().position(|t| *t == raw) {
            index
        } else {
            if self.textures.len() >= usize::from(self.pool_depth) {
                // More textures than the pool has buffers: WGC
                // reallocated. Start a new generation.
                let generation = self.pool.reallocate(usize::from(self.pool_depth));
                tracing::debug!(generation, "wgc pool textures changed");
                self.textures.clear();
            }
            self.textures.push(raw);
            self.textures.len() - 1
        };
        u32::try_from(index).unwrap_or(0)
    }

    fn shared_slot(
        &mut self,
        slot: u32,
        size: Size,
    ) -> Result<(ID3D11Texture2D, Arc<SharedHandle>), CaptureError> {
        let index = usize::try_from(slot).unwrap_or(usize::MAX);
        if self.slots.len() <= index {
            self.slots.resize_with(index + 1, || None);
        }
        if let Some(slot) = &self.slots[index] {
            return Ok((slot.texture.clone(), Arc::clone(&slot.handle)));
        }
        let format = if self.chosen.format == PixelFormat::Rgba16f {
            DXGI_FORMAT_R16G16B16A16_FLOAT
        } else {
            DXGI_FORMAT_B8G8R8A8_UNORM
        };
        let (texture, handle) = self.gpu.shared_texture(ID, size, format)?;
        let handle = Arc::new(handle);
        self.slots[index] = Some(Slot {
            texture: texture.clone(),
            handle: Arc::clone(&handle),
        });
        Ok((texture, handle))
    }

    fn meta(
        &self,
        frame: &Direct3D11CaptureFrame,
        size: Size,
        memory: BufferMemory,
        stride: u32,
        damage: Damage,
    ) -> FrameMeta {
        let mut meta = frame_meta(
            &self.source,
            FrameFacts {
                size,
                format: self.chosen.format,
                memory,
                stride,
                pts_ns: pts_of(frame),
                damage,
                cursor: None,
                orientation: crate::types::Orientation::UPRIGHT,
                producer: Some(GpuIdentity::Luid(self.gpu.luid)),
                protected: false,
            },
        );
        meta.tier = self.chosen.tier;
        meta
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the three delivery paths and the exhaustion policies of one frame"
    )]
    fn handle(
        &mut self,
        frame: Direct3D11CaptureFrame,
        skipped: &mut u32,
    ) -> Result<Delivery, CaptureError> {
        let current = self.capture.as_ref().map_or(Size::new(0, 0), |c| c.size);
        let content = frame.ContentSize().map_or(current, size_of);
        if content != current && !content.is_empty() {
            // This frame is still in an old-size buffer.
            let _closed = frame.Close();
            self.resize(content)?;
            return Ok(Delivery::Dropped);
        }
        let texture = texture_of(&frame)?;
        let desc = d3d::texture_desc(&texture);
        let size = Size::new(desc.Width, desc.Height);
        let damage = if self.dirty {
            dirty_of(&frame, size)
        } else {
            Damage::Full
        };
        let content_rect = Rect::from_size(content)
            .intersect(Rect::from_size(size))
            .unwrap_or_else(|| Rect::from_size(size));
        match self.path {
            Path::Cpu => {
                let image = self.readback.read(ID, &self.gpu, &texture)?;
                let _closed = frame.Close();
                let mut meta = self.meta(&frame, size, BufferMemory::Cpu, image.stride, damage);
                meta.content_rect = content_rect;
                meta.skipped_before = std::mem::take(skipped);
                Ok(self
                    .out
                    .deliver(meta, NativeBuffer::from_bytes(image.bytes)))
            }
            Path::Texture | Path::Shared => {
                let slot = if self.path == Path::Texture {
                    self.texture_slot(&texture)
                } else {
                    (0..u32::from(self.pool_depth))
                        .find(|s| !self.pool.is_held(*s))
                        .unwrap_or(0)
                };
                let lease = match self.pool.hold(slot) {
                    Ok(lease) => lease,
                    Err(err @ HoldError::SlotOutOfRange { .. }) => {
                        let _closed = frame.Close();
                        return Err(wgc_err("pool", err));
                    }
                    Err(HoldError::Exhausted(exhausted)) => {
                        self.out
                            .exhausted(exhausted, self.pool_depth, self.exhaustion);
                        *skipped = skipped.saturating_add(1);
                        return match self.exhaustion {
                            // WGC frames are released at once.
                            Exhaustion::Stall | Exhaustion::DropNewest => {
                                let _closed = frame.Close();
                                Ok(Delivery::Dropped)
                            }
                            Exhaustion::CopyToCpu => {
                                let image = self.readback.read(ID, &self.gpu, &texture)?;
                                let _closed = frame.Close();
                                let mut meta = self.meta(
                                    &frame,
                                    size,
                                    BufferMemory::Cpu,
                                    image.stride,
                                    Damage::Full,
                                );
                                meta.content_rect = content_rect;
                                meta.copied = Some(CopyStage::ExhaustionReadback);
                                meta.skipped_before = std::mem::take(skipped);
                                Ok(self
                                    .out
                                    .deliver(meta, NativeBuffer::from_bytes(image.bytes)))
                            }
                        };
                    }
                };
                let mut meta = self.meta(&frame, size, BufferMemory::D3d11Texture, 0, damage);
                meta.content_rect = content_rect;
                meta.skipped_before = std::mem::take(skipped);
                meta.buffer = lease.identity();
                let identity = lease.identity().into();
                let buffer = if self.path == Path::Texture {
                    let backing =
                        D3d11Frame::new(texture, self.gpu.device.clone(), None, self.gpu.luid);
                    // The frame goes back to the pool once the consumer
                    // dropped every clone.
                    NativeBuffer::from_platform(backing)
                        .with_identity(identity)
                        .with_release(move || {
                            let _closed = frame.Close();
                            drop(lease);
                        })
                } else {
                    let (target, handle) = self.shared_slot(lease.identity().slot, size)?;
                    self.gpu.copy(&target, &texture);
                    self.gpu.finish(ID, COPY_WAIT)?;
                    let _closed = frame.Close();
                    let backing = D3d11Frame::new(
                        target,
                        self.gpu.device.clone(),
                        Some(handle),
                        self.gpu.luid,
                    );
                    NativeBuffer::from_platform(backing)
                        .with_identity(identity)
                        .with_release(move || drop(lease))
                };
                Ok(self.out.deliver(meta, buffer))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FormatChoice, FormatTier};

    fn tier(memory: BufferMemory, format: PixelFormat) -> FormatTier {
        FormatTier::new(vec![FormatChoice {
            format,
            memory,
            modifiers: Vec::new(),
        }])
    }

    #[test]
    fn a_texture_tier_is_zero_copy() {
        let request = FormatRequest {
            tiers: vec![tier(BufferMemory::D3d11Texture, PixelFormat::Bgra8)],
        };
        let chosen = choose(&request, HdrPolicy::Sdr, false).expect("chosen");
        assert_eq!(chosen.copies, Vec::<CopyStage>::new());
        assert_eq!(path_of(&chosen), Path::Texture);
    }

    #[test]
    fn a_foreign_device_needs_a_tier_allowing_the_copy() {
        let plain = FormatRequest {
            tiers: vec![tier(BufferMemory::D3d11Texture, PixelFormat::Bgra8)],
        };
        assert_eq!(
            choose(&plain, HdrPolicy::Sdr, true),
            Err(CaptureError::unsatisfiable(Some(ID), Requirement::Gpu))
        );
        let allowing = FormatRequest {
            tiers: vec![
                tier(BufferMemory::D3d11Texture, PixelFormat::Bgra8)
                    .allowing(CopyStage::CrossDevice),
            ],
        };
        let chosen = choose(&allowing, HdrPolicy::Sdr, true).expect("chosen");
        assert_eq!(path_of(&chosen), Path::Shared);
    }

    #[test]
    fn cpu_frames_are_read_back() {
        let chosen = choose(&FormatRequest::cpu(), HdrPolicy::Sdr, false).expect("chosen");
        assert_eq!(chosen.copies, vec![CopyStage::GpuReadback]);
        assert_eq!(path_of(&chosen), Path::Cpu);
    }

    #[test]
    fn hdr_takes_fp16_inside_the_callers_tier() {
        let request = FormatRequest {
            tiers: vec![FormatTier::new(vec![
                FormatChoice {
                    format: PixelFormat::Bgra8,
                    memory: BufferMemory::D3d11Texture,
                    modifiers: Vec::new(),
                },
                FormatChoice {
                    format: PixelFormat::Rgba16f,
                    memory: BufferMemory::D3d11Texture,
                    modifiers: Vec::new(),
                },
            ])],
        };
        assert_eq!(
            choose(&request, HdrPolicy::PreferHdr, false)
                .expect("hdr")
                .format,
            PixelFormat::Rgba16f
        );
        assert_eq!(
            choose(&request, HdrPolicy::Sdr, false).expect("sdr").format,
            PixelFormat::Bgra8
        );
        assert!(choose(&FormatRequest::cpu(), HdrPolicy::RequireHdr, false).is_err());
    }

    #[test]
    fn borderless_needs_the_policy() {
        let features = Features {
            cursor_toggle: true,
            border: true,
            dirty: true,
            min_interval: true,
            secondary: true,
        };
        let require = WindowsOptions {
            borderless: Borderless::Require,
        };
        assert!(matches!(
            borderless(require, false, features),
            Err(CaptureError::PolicyDenied(_))
        ));
        let prefer = WindowsOptions {
            borderless: Borderless::Prefer,
        };
        assert_eq!(
            borderless(prefer, false, features),
            Ok((false, BorderState::Shown))
        );
        let unsupported = Features {
            border: false,
            ..features
        };
        assert_eq!(
            borderless(require, true, unsupported),
            Err(CaptureError::unsatisfiable(
                Some(ID),
                Requirement::Borderless
            ))
        );
    }
}
