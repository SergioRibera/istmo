//! DXGI Desktop Duplication backend (`windows-dxgi`): monitors only.
//!
//! - One duplication per output per process, on a device of the adapter
//!   that drives the output (`GpuPreference::Same` naming another adapter
//!   is `Unsatisfiable(Gpu)`).
//! - Only one acquired frame at a time: each frame is copied into the
//!   plugin's pool before `ReleaseFrame` (`CopyStage::DxgiMandatory`, so
//!   DXGI is never copy-free). Pool textures are shareable
//!   (`D3D11_RESOURCE_MISC_SHARED_NTHANDLE`), so every frame carries an
//!   NT handle at no extra copy; the CPU tier reads the acquired image
//!   back through a staging texture instead (`GpuReadback`).
//! - Dirty and move rectangles → damage; the pointer is never in the
//!   image: `CursorMode::Metadata` carries its position and hotspot.
//! - Rotated outputs arrive in scan-out orientation:
//!   `DXGI_OUTDUPL_DESC.Rotation` → `FrameMeta::orientation`.
//! - `ProtectedContentMaskedOut` → `FrameMeta::protected_content`.
//! - `DXGI_ERROR_ACCESS_LOST` (mode change, desktop switch) re-creates the
//!   duplication, bumps `pool_generation` and sends `Renegotiated`; the
//!   secure desktop (UAC, lock screen: `E_ACCESSDENIED`) is
//!   `Paused(SecureDesktop)` until it goes away.

#![allow(
    unsafe_code,
    reason = "DXGI Desktop Duplication COM calls are unsafe in the windows crate"
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use istmo_core::NativeBuffer;
use windows::Win32::Foundation::{E_ACCESSDENIED, E_INVALIDARG, RECT};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R10G10B10A2_UNORM,
    DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_ROTATE90,
    DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_NOT_CURRENTLY_AVAILABLE, DXGI_ERROR_SESSION_DISCONNECTED,
    DXGI_ERROR_UNSUPPORTED, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_DESC, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTDUPL_MOVE_RECT, DXGI_OUTDUPL_POINTER_SHAPE_INFO, IDXGIOutput1, IDXGIOutput5,
    IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface as _;

use crate::d3d11::{D3d11Frame, SharedHandle};
use crate::pixels::OrientationMap;
use crate::pool::{HoldError, PoolAccounting};
use crate::select::{self, Chosen, Offer};
use crate::types::{
    Availability, BackendCaps, BackendId, BorderState, BufferMemory, CapsProvenance, CaptureError,
    CaptureEvent, CompositeCaps, CompositeSupport, ConsentModel, CopyStage, CursorMode,
    CursorState, Damage, DamageSupport, Exhaustion, FormatCaps, FormatRequest, FrameRate,
    GpuIdentity, GpuPreference, HdrPolicy, NegotiatedStream, Orientation, OrientationSupport,
    PauseReason, PixelFormat, Point, PoolCaps, ProtectedContent, Provenance, Rect, RegionCrop,
    Requirement, Rotation, Scaling, SessionUpdate, Size, Source, SourceKind, SourceList,
    SourceListing, SourceSelector, Strength, WindowCaptureCaps,
};

use super::super::backend::{
    Backend, BackendSession, Captured, ScreenshotPlan, Shot, StreamPlan, Target,
};
use super::super::output::{Delivery, FrameOut};
use super::super::watch::SourceFeed;
use super::common::{
    FrameFacts, StreamFacts, damage_of, frame_meta, negotiated, owed_damage, period,
};
use super::d3d::{self, Gpu, LateCopies, Readback};
use super::listing::{self, Target as WinTarget};
use super::os::{self, PhysicalPixels};

const ID: BackendId = BackendId::WindowsDxgi;

pub(crate) fn backend() -> Arc<dyn Backend> {
    Arc::new(DxgiBackend)
}

/// Pool limits: each slot is a full-size GPU texture.
const POOL: PoolCaps = PoolCaps {
    min_depth: 2,
    max_depth: 8,
    default_depth: 3,
    exhaustion: Vec::new(),
};
const CURSOR_MODES: [CursorMode; 2] = [CursorMode::Hidden, CursorMode::Metadata];
/// Longest wait for one frame, so the stop flag is seen.
const ACQUIRE_WAIT: Duration = Duration::from_millis(100);
/// How long a screenshot waits for its first image.
const SCREENSHOT_WAIT: Duration = Duration::from_secs(2);
/// How long the GPU may take to finish a copy into the pool.
const COPY_WAIT: Duration = Duration::from_millis(100);
/// A screenshot has one frame: it waits longer before giving up.
const SHOT_COPY_WAIT: Duration = Duration::from_secs(1);
/// How often a lost duplication is re-created.
const RETRY: Duration = Duration::from_millis(250);
/// Re-creation attempts after `ACCESS_LOST` before the session fails
/// (the secure desktop pauses instead, without a limit).
const RETRIES: u32 = 40;
/// How long dropping a session waits for its thread.
const JOIN_WAIT: Duration = Duration::from_secs(2);

fn hr(operation: &str, err: &windows_core::Error) -> CaptureError {
    CaptureError::os(ID, operation, i64::from(err.code().0))
}

fn dxgi_err(step: &str, message: impl std::fmt::Display) -> CaptureError {
    CaptureError::protocol(ID, step, message)
}

/// `DXGI_OUTDUPL_DESC.Rotation` → what the buffer needs to be shown
/// upright.
const fn orientation_of(rotation: DXGI_MODE_ROTATION) -> Orientation {
    let rotation = match rotation {
        DXGI_MODE_ROTATION_ROTATE90 => Rotation::R90,
        DXGI_MODE_ROTATION_ROTATE180 => Rotation::R180,
        DXGI_MODE_ROTATION_ROTATE270 => Rotation::R270,
        _ => Rotation::R0,
    };
    Orientation {
        mirrored: false,
        rotation,
    }
}

/// Whether `IDXGIOutput5::DuplicateOutput1` (HDR / FP16 duplication,
/// Windows 10 1703+) exists on the primary output.
fn fp16_supported() -> bool {
    d3d::adapters(ID)
        .ok()
        .and_then(|adapters| adapters.into_iter().find(|a| !a.software))
        // SAFETY: a live adapter; an index past the end fails.
        .and_then(|a| unsafe { a.dxgi.EnumOutputs(0) }.ok())
        .is_some_and(|output| output.cast::<IDXGIOutput5>().is_ok())
}

fn formats(fp16: bool) -> Vec<PixelFormat> {
    if fp16 {
        vec![PixelFormat::Bgra8, PixelFormat::Rgba16f, PixelFormat::Xb30]
    } else {
        vec![PixelFormat::Bgra8]
    }
}

fn offer_list(formats: &[PixelFormat]) -> Vec<Offer> {
    formats
        .iter()
        .flat_map(|format| {
            [
                Offer {
                    format: *format,
                    memory: BufferMemory::D3d11Texture,
                    modifiers: Vec::new(),
                    copies: vec![CopyStage::DxgiMandatory],
                },
                Offer {
                    format: *format,
                    memory: BufferMemory::Cpu,
                    modifiers: Vec::new(),
                    copies: vec![CopyStage::DxgiMandatory, CopyStage::GpuReadback],
                },
            ]
        })
        .collect()
}

fn caps() -> BackendCaps {
    let fp16 = fp16_supported();
    let mut caps = BackendCaps::new(ID);
    caps.source_kinds = vec![SourceKind::Monitor];
    caps.listing = SourceListing::Enumerable;
    caps.consent = ConsentModel::NoneNeeded;
    caps.video = offer_list(&formats(fp16))
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
    caps.cursor = CURSOR_MODES.to_vec();
    caps.damage = DamageSupport::Rects;
    caps.orientation = OrientationSupport::InMeta;
    caps.colour.hdr = fp16;
    if fp16 {
        caps.colour.colours.push(super::common::SCRGB);
    }
    caps.producer = d3d::adapters(ID)
        .ok()
        .and_then(|a| a.into_iter().find(|a| !a.software))
        .map(|a| GpuIdentity::Luid(a.luid));
    caps.pool = PoolCaps {
        exhaustion: vec![
            Exhaustion::Stall,
            Exhaustion::DropNewest,
            Exhaustion::CopyToCpu,
        ],
        ..POOL
    };
    caps.copies = vec![
        CopyStage::DxgiMandatory,
        CopyStage::GpuReadback,
        CopyStage::ExhaustionReadback,
    ];
    // DXGI says which frames had protected content masked out.
    caps.protected_content = ProtectedContent::Reported;
    caps.window = WindowCaptureCaps::default();
    // One duplication of an output per process.
    caps.concurrency = Some(1);
    caps.provenance = CapsProvenance {
        source_kinds: Provenance::Table,
        listing: Provenance::Probe,
        video: Provenance::Probe,
        cursor: Provenance::Table,
        damage: Provenance::Table,
        colour: Provenance::Probe,
        audio: Provenance::Table,
        producer: Provenance::Probe,
        sync: Provenance::Table,
        persistence: Provenance::Table,
    };
    caps
}

fn listing_of() -> SourceList {
    SourceList {
        sources: listing::monitors(ID, true)
            .into_iter()
            .map(|m| m.source)
            .collect(),
        listing: SourceListing::Enumerable,
    }
}

/// Why a duplication could not be made (now).
enum DupError {
    /// The secure desktop or another session owns the screen: paused,
    /// retried without a limit.
    NotNow(PauseReason, CaptureError),
    /// The output is duplicated elsewhere (this process, or the
    /// four-process limit), or its mode cannot be duplicated now: retry a
    /// bounded number of times.
    Busy(CaptureError),
    /// The monitor is gone.
    Lost,
    Fatal(CaptureError),
}

/// What a failed `DuplicateOutput[1]` means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DupFailure {
    /// `E_ACCESSDENIED` (UAC, lock screen) or another session: wait it
    /// out, paused for this reason.
    NotNow(PauseReason),
    /// `E_INVALIDARG`: this process already duplicates the output (a
    /// screenshot during a stream, or a lost duplication still alive).
    OwnBusy,
    /// `DXGI_ERROR_NOT_CURRENTLY_AVAILABLE`: too many processes
    /// duplicate it.
    OthersBusy,
    /// `DXGI_ERROR_UNSUPPORTED`: a mode duplication does not handle (8 bpp,
    /// DWM off) or the other GPU of a hybrid laptop. Not a secure
    /// desktop, and possibly for good.
    Unsupported,
    Fatal,
}

fn classify(code: windows_core::HRESULT) -> DupFailure {
    if code == E_ACCESSDENIED {
        DupFailure::NotNow(PauseReason::SecureDesktop)
    } else if code == DXGI_ERROR_SESSION_DISCONNECTED {
        DupFailure::NotNow(PauseReason::SystemPaused)
    } else if code == E_INVALIDARG {
        DupFailure::OwnBusy
    } else if code == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE {
        DupFailure::OthersBusy
    } else if code == DXGI_ERROR_UNSUPPORTED {
        DupFailure::Unsupported
    } else {
        DupFailure::Fatal
    }
}

fn dup_error(err: &windows_core::Error) -> DupError {
    match classify(err.code()) {
        DupFailure::NotNow(reason) => DupError::NotNow(reason, hr("DuplicateOutput", err)),
        DupFailure::OwnBusy => DupError::Busy(dxgi_err(
            "DuplicateOutput",
            "the output is already duplicated in this process",
        )),
        DupFailure::OthersBusy => DupError::Busy(dxgi_err(
            "DuplicateOutput",
            "too many processes duplicate this output",
        )),
        DupFailure::Unsupported => DupError::Busy(dxgi_err(
            "DuplicateOutput",
            "DXGI_ERROR_UNSUPPORTED: this display mode or adapter cannot be duplicated \
             (8 bpp or DWM off, or the other GPU of a hybrid system)",
        )),
        DupFailure::Fatal => DupError::Fatal(hr("DuplicateOutput", err)),
    }
}

impl From<DupError> for CaptureError {
    fn from(err: DupError) -> Self {
        match err {
            DupError::NotNow(_, err) | DupError::Busy(err) | DupError::Fatal(err) => err,
            DupError::Lost => Self::SourceLost,
        }
    }
}

/// A live duplication of one output.
struct Dup {
    gpu: Gpu,
    /// `None` once released: a lost duplication must be released
    /// before the output can be duplicated again.
    output: Option<IDXGIOutputDuplication>,
    size: Size,
    format: PixelFormat,
    orientation: Orientation,
}

impl Dup {
    fn new(device: &str, hdr: HdrPolicy, preference: GpuPreference) -> Result<Self, DupError> {
        let (adapter, output) = d3d::output_for(ID, device)
            .map_err(DupError::Fatal)?
            .ok_or(DupError::Lost)?;
        if let GpuPreference::Same(GpuIdentity::Luid(luid)) = preference
            && luid != adapter.luid
        {
            return Err(DupError::Fatal(CaptureError::unsatisfiable(
                Some(ID),
                Requirement::Gpu,
            )));
        }
        let gpu = Gpu::new(ID, Some(&adapter)).map_err(DupError::Fatal)?;
        let output5 = (hdr != HdrPolicy::Sdr)
            .then(|| output.cast::<IDXGIOutput5>().ok())
            .flatten();
        if hdr == HdrPolicy::RequireHdr && output5.is_none() {
            return Err(DupError::Fatal(CaptureError::unsatisfiable(
                Some(ID),
                Requirement::Hdr,
            )));
        }
        let plain = || {
            let output1 = output
                .cast::<IDXGIOutput1>()
                .map_err(|e| DupError::Fatal(hr("IDXGIOutput1", &e)))?;
            // SAFETY: the device is on the output's adapter.
            unsafe { output1.DuplicateOutput(&gpu.device) }.map_err(|err| dup_error(&err))
        };
        let dup = if let Some(output5) = output5 {
            let supported: [DXGI_FORMAT; 3] = [
                DXGI_FORMAT_R16G16B16A16_FLOAT,
                DXGI_FORMAT_R10G10B10A2_UNORM,
                DXGI_FORMAT_B8G8R8A8_UNORM,
            ];
            // SAFETY: the device is on the output's adapter; the format
            // list is live for the call.
            match unsafe { output5.DuplicateOutput1(&gpu.device, 0, &supported) } {
                Ok(dup) => dup,
                // `DuplicateOutput1` refuses some hosts (DPI awareness,
                // drivers) where the BGRA `DuplicateOutput` works.
                Err(err)
                    if classify(err.code()) == DupFailure::Unsupported
                        && hdr != HdrPolicy::RequireHdr =>
                {
                    tracing::debug!(%err, "DuplicateOutput1 unsupported; trying DuplicateOutput");
                    plain()?
                }
                Err(err) => return Err(dup_error(&err)),
            }
        } else {
            plain()?
        };
        // SAFETY: `dup` is live.
        let desc: DXGI_OUTDUPL_DESC = unsafe { dup.GetDesc() };
        let format = d3d::pixel_format(desc.ModeDesc.Format).ok_or_else(|| {
            DupError::Fatal(dxgi_err(
                "DuplicateOutput",
                format!("DXGI format {}", desc.ModeDesc.Format.0),
            ))
        })?;
        if hdr == HdrPolicy::RequireHdr && format != PixelFormat::Rgba16f {
            return Err(DupError::Fatal(CaptureError::unsatisfiable(
                Some(ID),
                Requirement::Hdr,
            )));
        }
        Ok(Self {
            gpu,
            output: Some(dup),
            size: Size::new(desc.ModeDesc.Width, desc.ModeDesc.Height),
            format,
            orientation: orientation_of(desc.Rotation),
        })
    }

    /// Release the duplication (after `ACCESS_LOST`); `acquire` then
    /// answers `ACCESS_LOST` until a new one replaces it.
    fn release(&mut self) {
        self.output = None;
    }

    /// The next frame, or `None` on timeout.
    fn acquire(&self, timeout: Duration) -> Result<Option<Acquired>, windows_core::Error> {
        let Some(output) = &self.output else {
            return Err(DXGI_ERROR_ACCESS_LOST.into());
        };
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: both out values are live locals; the previous frame was
        // released (every `Acquired` releases on drop).
        match unsafe { output.AcquireNextFrame(ms, &raw mut info, &raw mut resource) } {
            Ok(()) => Ok(Some(Acquired {
                dup: output.clone(),
                info,
                resource,
            })),
            Err(err) if err.code() == DXGI_ERROR_WAIT_TIMEOUT => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// One acquired frame, released on drop.
struct Acquired {
    dup: IDXGIOutputDuplication,
    info: DXGI_OUTDUPL_FRAME_INFO,
    resource: Option<IDXGIResource>,
}

impl Acquired {
    /// Whether the desktop image changed (not only the pointer).
    const fn has_image(&self) -> bool {
        self.info.LastPresentTime != 0
    }

    fn texture(&self) -> Result<ID3D11Texture2D, CaptureError> {
        self.resource
            .as_ref()
            .ok_or_else(|| dxgi_err("AcquireNextFrame", "no desktop resource"))?
            .cast::<ID3D11Texture2D>()
            .map_err(|e| hr("ID3D11Texture2D", &e))
    }

    /// Dirty and moved rectangles, in buffer pixels.
    fn damage(&self, size: Size) -> Damage {
        let total = self.info.TotalMetadataBufferSize;
        if total == 0 {
            return Damage::Full;
        }
        let mut rects: Vec<Rect> = Vec::new();
        let move_len =
            usize::try_from(total).unwrap_or(0) / std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>() + 1;
        let mut moves = vec![DXGI_OUTDUPL_MOVE_RECT::default(); move_len];
        let mut used = 0_u32;
        let move_bytes =
            u32::try_from(moves.len() * std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>()).unwrap_or(0);
        // SAFETY: `moves` holds `move_bytes` bytes; `used` receives how
        // many were written.
        if unsafe {
            self.dup
                .GetFrameMoveRects(move_bytes, moves.as_mut_ptr(), &raw mut used)
        }
        .is_err()
        {
            return Damage::Full;
        }
        let count =
            usize::try_from(used).unwrap_or(0) / std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>();
        rects.extend(moves.iter().take(count).map(|m| rect_of(m.DestinationRect)));
        let dirty_len = usize::try_from(total).unwrap_or(0) / std::mem::size_of::<RECT>() + 1;
        let mut dirty = vec![RECT::default(); dirty_len];
        let dirty_bytes = u32::try_from(dirty.len() * std::mem::size_of::<RECT>()).unwrap_or(0);
        // SAFETY: `dirty` holds `dirty_bytes` bytes; `used` receives how
        // many were written.
        if unsafe {
            self.dup
                .GetFrameDirtyRects(dirty_bytes, dirty.as_mut_ptr(), &raw mut used)
        }
        .is_err()
        {
            return Damage::Full;
        }
        let count = usize::try_from(used).unwrap_or(0) / std::mem::size_of::<RECT>();
        rects.extend(dirty.iter().take(count).map(|r| rect_of(*r)));
        damage_of(rects, size)
    }

    /// The pointer's hotspot from a new shape, if the frame carries one.
    fn pointer_hotspot(&self) -> Option<Point> {
        let len = self.info.PointerShapeBufferSize;
        if len == 0 {
            return None;
        }
        let mut buffer = vec![0_u8; usize::try_from(len).ok()?];
        let mut required = 0_u32;
        let mut shape = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        // SAFETY: `buffer` holds `len` bytes; the out values are live.
        unsafe {
            self.dup.GetFramePointerShape(
                len,
                buffer.as_mut_ptr().cast(),
                &raw mut required,
                &raw mut shape,
            )
        }
        .ok()?;
        Some(Point {
            x: shape.HotSpot.x,
            y: shape.HotSpot.y,
        })
    }
}

impl Drop for Acquired {
    fn drop(&mut self) {
        // SAFETY: releases the frame AcquireNextFrame handed out; nothing
        // reads the desktop resource after this.
        let _released = unsafe { self.dup.ReleaseFrame() };
    }
}

const fn rect_of(r: RECT) -> Rect {
    Rect::new(
        r.left,
        r.top,
        r.right.saturating_sub(r.left).unsigned_abs(),
        r.bottom.saturating_sub(r.top).unsigned_abs(),
    )
}

/// The pointer as the last frames reported it, desktop space.
#[derive(Debug, Clone, Copy, Default)]
struct Pointer {
    position: Option<Point>,
    hotspot: Point,
    visible: bool,
}

impl Pointer {
    fn update(&mut self, frame: &Acquired) {
        if frame.info.LastMouseUpdateTime != 0 {
            self.visible = frame.info.PointerPosition.Visible.as_bool();
            self.position = Some(Point {
                x: frame.info.PointerPosition.Position.x,
                y: frame.info.PointerPosition.Position.y,
            });
        }
        if let Some(hotspot) = frame.pointer_hotspot() {
            self.hotspot = hotspot;
        }
    }

    /// The hotspot in buffer pixels of a `size` buffer needing
    /// `orientation`.
    fn state(&self, size: Size, orientation: Orientation) -> Option<CursorState> {
        let at = self.position?;
        let map = OrientationMap { size, orientation };
        let upright = map.output_size();
        let x = at.x.saturating_add(self.hotspot.x);
        let y = at.y.saturating_add(self.hotspot.y);
        let inside = u32::try_from(x)
            .ok()
            .filter(|x| *x < upright.width)
            .zip(u32::try_from(y).ok().filter(|y| *y < upright.height));
        let position = inside.map_or(Point { x, y }, |(u, v)| {
            let (bx, by) = map.source(u, v);
            Point {
                x: i32::try_from(bx).unwrap_or(x),
                y: i32::try_from(by).unwrap_or(y),
            }
        });
        Some(CursorState {
            position,
            hotspot: self.hotspot,
            visible: self.visible && inside.is_some(),
        })
    }
}

/// How the chosen tier delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Path {
    /// Copied into a shareable pool texture.
    Texture,
    /// Read back into CPU memory.
    Cpu,
}

fn choose(formats: &FormatRequest, format: PixelFormat) -> Result<(Chosen, Path), CaptureError> {
    let chosen = select::negotiate(formats, &offer_list(&[format]), 0)
        .ok_or_else(|| CaptureError::unsatisfiable(Some(ID), Requirement::Formats))?;
    let path = if chosen.memory == BufferMemory::Cpu {
        Path::Cpu
    } else {
        Path::Texture
    };
    Ok((chosen, path))
}

fn device_of(source: &Source) -> Result<String, CaptureError> {
    match listing::target_of(ID, source)? {
        WinTarget::Monitor(device) => Ok(device),
        WinTarget::Window(_) => Err(CaptureError::unsatisfiable(
            Some(ID),
            Requirement::Kind(SourceKind::Window),
        )),
    }
}

fn stream_facts(
    dup: &Dup,
    chosen: &Chosen,
    pool_depth: u8,
    exhaustion: Exhaustion,
    cursor: CursorMode,
) -> StreamFacts {
    StreamFacts {
        backend: ID,
        size: dup.size,
        format: dup.format,
        memory: chosen.memory,
        tier: chosen.tier,
        copies: chosen.copies.clone(),
        pool_depth,
        exhaustion,
        orientation: OrientationSupport::InMeta,
        damage: DamageSupport::Rects,
        cursor,
        producer: Some(GpuIdentity::Luid(dup.gpu.luid)),
        border: BorderState::NotApplicable,
    }
}

fn frame_facts(
    dup: &Dup,
    memory: BufferMemory,
    stride: u32,
    info: &DXGI_OUTDUPL_FRAME_INFO,
    damage: Damage,
    cursor: Option<CursorState>,
) -> FrameFacts {
    FrameFacts {
        size: dup.size,
        format: dup.format,
        memory,
        stride,
        pts_ns: os::qpc_ticks_to_ns(info.LastPresentTime),
        damage,
        cursor,
        orientation: dup.orientation,
        producer: Some(GpuIdentity::Luid(dup.gpu.luid)),
        protected: info.ProtectedContentMaskedOut.as_bool(),
    }
}

#[derive(Debug)]
struct DxgiBackend;

impl Backend for DxgiBackend {
    fn id(&self) -> BackendId {
        ID
    }

    fn availability(&self) -> Availability {
        match d3d::adapters(ID) {
            Ok(adapters) => {
                let outputs = adapters.iter().any(|a| {
                    // SAFETY: a live adapter; an index past the end fails.
                    unsafe { a.dxgi.EnumOutputs(0) }.is_ok()
                });
                if outputs {
                    Availability::Ready
                } else {
                    Availability::NotOnThisSession(
                        "no DXGI output (a remote or headless session)".to_owned(),
                    )
                }
            }
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
            SourceSelector::UnderCursor => listing::under_cursor(ID, true),
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
        let mode = select::cursor_mode(plan.cursor, &CURSOR_MODES);
        if plan.cursor.strength == Strength::Require && mode != plan.cursor.mode {
            return Err(CaptureError::unsatisfiable(Some(ID), Requirement::Cursor));
        }
        let mut frames = Vec::with_capacity(sources.len());
        let mut first = None;
        for source in sources {
            let dup = Dup::new(&device_of(source)?, plan.hdr, GpuPreference::Auto)?;
            let (chosen, path) = choose(&plan.formats, dup.format)?;
            let mut pointer = Pointer::default();
            let deadline = Instant::now() + SCREENSHOT_WAIT;
            let (meta, buffer) = loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(dxgi_err("AcquireNextFrame", "no desktop image in time"));
                }
                let Some(frame) = dup
                    .acquire(remaining.min(ACQUIRE_WAIT))
                    .map_err(|e| hr("AcquireNextFrame", &e))?
                else {
                    continue;
                };
                pointer.update(&frame);
                if !frame.has_image() {
                    continue;
                }
                let cursor = (mode == CursorMode::Metadata)
                    .then(|| pointer.state(dup.size, dup.orientation))
                    .flatten();
                let texture = frame.texture()?;
                break match path {
                    Path::Cpu => {
                        let image = Readback::default().read(ID, &dup.gpu, &texture)?;
                        let meta = frame_meta(
                            source,
                            frame_facts(
                                &dup,
                                BufferMemory::Cpu,
                                image.stride,
                                &frame.info,
                                Damage::Full,
                                cursor,
                            ),
                        );
                        (meta, NativeBuffer::from_bytes(image.bytes))
                    }
                    Path::Texture => {
                        let format = d3d::dxgi_format(dup.format)
                            .ok_or_else(|| dxgi_err("screenshot", "format"))?;
                        let (copy, handle) = dup.gpu.shared_texture(ID, dup.size, format)?;
                        dup.gpu.copy(&copy, &texture);
                        dup.gpu.finish(ID, SHOT_COPY_WAIT)?;
                        let meta = frame_meta(
                            source,
                            frame_facts(
                                &dup,
                                BufferMemory::D3d11Texture,
                                0,
                                &frame.info,
                                Damage::Full,
                                cursor,
                            ),
                        );
                        let backing = D3d11Frame::new(
                            copy,
                            dup.gpu.device.clone(),
                            Some(Arc::new(handle)),
                            dup.gpu.luid,
                        );
                        (meta, NativeBuffer::from_platform(backing))
                    }
                };
            };
            let mut meta = meta;
            meta.tier = chosen.tier;
            if first.is_none() {
                first = Some(negotiated(
                    source,
                    stream_facts(&dup, &chosen, 1, Exhaustion::Stall, mode),
                ));
            }
            let mut captured = source.clone();
            captured.orientation = dup.orientation;
            frames.push(Captured {
                meta,
                buffer,
                source: captured,
            });
        }
        let negotiated = first.ok_or_else(|| dxgi_err("screenshot", "no source"))?;
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
        let device = device_of(&plan.source)?;
        // Same DPI context as the screenshot and the capture thread:
        // physical pixels, which `DuplicateOutput1` wants.
        let dup = {
            let _pixels = PhysicalPixels::enter();
            Dup::new(&device, plan.hdr, plan.gpu)?
        };
        let (chosen, path) = choose(&plan.formats, dup.format)?;
        let mode = select::cursor_mode(plan.cursor, &CURSOR_MODES);
        // A metadata cursor re-sends the last frame on pointer-only
        // updates: that frame is kept, one more buffer.
        let depth = plan
            .pool_depth
            .saturating_add(u8::from(mode == CursorMode::Metadata))
            .clamp(POOL.min_depth, POOL.max_depth);
        let mut source = plan.source.clone();
        source.orientation = dup.orientation;
        let negotiated = negotiated(
            &source,
            stream_facts(&dup, &chosen, depth, plan.exhaustion, mode),
        );
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            cursor: Mutex::new(mode),
            negotiated: Mutex::new(negotiated.clone()),
        });
        let worker = Worker {
            device,
            hdr: plan.hdr,
            gpu_preference: plan.gpu,
            formats: plan.formats.clone(),
            dup,
            path,
            chosen,
            source,
            out,
            shared: Arc::clone(&shared),
            pool: PoolAccounting::new(usize::from(depth)),
            slots: Vec::new(),
            pool_depth: depth,
            exhaustion: plan.exhaustion,
            readback: Readback::default(),
            pointer: Pointer::default(),
            late: LateCopies::default(),
            last: None,
        };
        let (done_tx, done) = flume::bounded(1);
        let thread = std::thread::Builder::new()
            .name("istmo-screen-capture-dxgi".to_owned())
            .spawn(move || {
                let _pixels = PhysicalPixels::enter();
                worker.run();
                let _finished = done_tx.send(());
            })
            .map_err(|e| dxgi_err("spawn capture thread", e))?;
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
            // A new tier needs a new pool and path: reopen the session.
            SessionUpdate::Formats(_)
            | SessionUpdate::Fps(_)
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
            tracing::warn!("dxgi capture thread did not stop in time; detached");
        }
    }
}

/// One pool slot: a shareable texture and its NT handle.
struct Slot {
    texture: ID3D11Texture2D,
    handle: Arc<SharedHandle>,
}

struct Worker {
    device: String,
    hdr: HdrPolicy,
    gpu_preference: GpuPreference,
    formats: FormatRequest,
    dup: Dup,
    path: Path,
    chosen: Chosen,
    source: Source,
    out: FrameOut,
    shared: Arc<Shared>,
    pool: PoolAccounting,
    slots: Vec<Option<Slot>>,
    pool_depth: u8,
    exhaustion: Exhaustion,
    readback: Readback,
    pointer: Pointer,
    late: LateCopies,
    /// The last delivered frame, re-sent on pointer-only updates under
    /// `CursorMode::Metadata`.
    last: Option<(crate::types::FrameMeta, NativeBuffer)>,
}

impl Worker {
    fn period(&self) -> Duration {
        period(self.out.fps(), self.source.refresh_mhz)
    }

    fn cursor(&self) -> CursorMode {
        *self
            .shared
            .cursor
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Re-create the duplication after `ACCESS_LOST`: pauses while the
    /// secure desktop is up, bumps the pool generation and renegotiates.
    fn recreate(&mut self) -> Result<(), CaptureError> {
        // DXGI: "the application must release the IDXGIOutputDuplication
        // interface and create a new one"; a live one makes
        // `DuplicateOutput` answer `E_INVALIDARG`.
        self.last = None;
        self.dup.release();
        let mut paused = false;
        let mut attempts = 0_u32;
        let dup = loop {
            if self.shared.stop.load(Ordering::SeqCst) || self.out.is_stopped() {
                return Ok(());
            }
            match Dup::new(&self.device, self.hdr, self.gpu_preference) {
                Ok(dup) => break dup,
                Err(DupError::NotNow(reason, err)) => {
                    if !paused {
                        tracing::debug!(%err, "dxgi duplication unavailable; paused");
                        self.out.event(CaptureEvent::Paused(reason));
                        paused = true;
                    }
                }
                Err(DupError::Lost) => return Err(CaptureError::SourceLost),
                Err(DupError::Busy(err) | DupError::Fatal(err)) => {
                    attempts += 1;
                    if attempts >= RETRIES {
                        return Err(err);
                    }
                }
            }
            std::thread::sleep(RETRY);
        };
        if paused {
            self.out.event(CaptureEvent::Resumed);
        }
        let resized = dup.size != self.dup.size;
        let (chosen, path) = choose(&self.formats, dup.format)?;
        self.dup = dup;
        self.chosen = chosen;
        self.path = path;
        self.slots.clear();
        self.readback.reset();
        self.last = None;
        let generation = self.pool.reallocate(usize::from(self.pool_depth));
        tracing::debug!(generation, "dxgi duplication re-created");
        self.source.orientation = self.dup.orientation;
        if resized {
            self.out.event(CaptureEvent::SourceResized(self.dup.size));
        }
        let mut fresh = super::common::negotiated(
            &self.source,
            stream_facts(
                &self.dup,
                &self.chosen,
                self.pool_depth,
                self.exhaustion,
                self.cursor(),
            ),
        );
        fresh.fps = self.out.fps();
        self.shared
            .negotiated
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone_from(&fresh);
        self.out.event(CaptureEvent::Renegotiated(fresh));
        Ok(())
    }

    fn run(mut self) {
        let mut last_capture: Option<Instant> = None;
        let mut skipped: u32 = 0;
        let mut damage = Damage::Full;
        loop {
            if self.shared.stop.load(Ordering::SeqCst) || self.out.is_stopped() {
                break;
            }
            if !self.out.wants_frames() {
                // Paused: drain so DXGI keeps accumulating, but deliver
                // nothing; the next delivered frame is fully damaged.
                if let Ok(Some(frame)) = self.dup.acquire(Duration::ZERO) {
                    drop(frame);
                }
                damage = Damage::Full;
                self.last = None;
                self.out.wait_while_paused(ACQUIRE_WAIT);
                continue;
            }
            let frame = match self.dup.acquire(ACQUIRE_WAIT) {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
                Err(err) if err.code() == DXGI_ERROR_ACCESS_LOST => {
                    if let Err(err) = self.recreate() {
                        self.out.fail(err);
                        break;
                    }
                    damage = Damage::Full;
                    continue;
                }
                Err(err) => {
                    self.out
                        .fail(self.dup.gpu.failure(ID, "AcquireNextFrame", &err));
                    break;
                }
            };
            self.pointer.update(&frame);
            let cursor_mode = self.cursor();
            if !frame.has_image() {
                // Pointer only: re-send the last frame with the new pointer.
                drop(frame);
                if cursor_mode == CursorMode::Metadata
                    && let Some((meta, buffer)) = self.last.clone()
                {
                    let mut meta = meta;
                    meta.cursor = self.pointer.state(self.dup.size, self.dup.orientation);
                    meta.damage = Damage::Unchanged;
                    meta.pts_ns = os::qpc_now_ns();
                    if self.out.deliver(meta, buffer) == Delivery::Closed {
                        break;
                    }
                }
                continue;
            }
            damage = damage.merge(frame.damage(self.dup.size));
            skipped = skipped.saturating_add(frame.info.AccumulatedFrames.saturating_sub(1));
            // Paced at the source: frames inside the period are merged
            // into the next one instead of copied.
            let early = self.out.fps() != FrameRate::Native
                && last_capture.is_some_and(|at| {
                    let period = self.period();
                    at.elapsed() + period / 20 < period
                });
            if early {
                skipped = skipped.saturating_add(1);
                continue;
            }
            last_capture = Some(Instant::now());
            let sent = std::mem::replace(&mut damage, Damage::Unchanged);
            match self.deliver(&frame, sent.clone(), cursor_mode, &mut skipped) {
                Ok(Delivery::Closed) => break,
                Ok(delivery) => damage = owed_damage(delivery, sent),
                Err(err) => {
                    drop(frame);
                    self.out.fail(err);
                    break;
                }
            }
        }
        self.last = None;
    }

    fn slot(&mut self, index: u32) -> Result<(ID3D11Texture2D, Arc<SharedHandle>), CaptureError> {
        let index = usize::try_from(index).unwrap_or(usize::MAX);
        if self.slots.len() <= index {
            self.slots.resize_with(index + 1, || None);
        }
        if let Some(slot) = &self.slots[index] {
            return Ok((slot.texture.clone(), Arc::clone(&slot.handle)));
        }
        let format = d3d::dxgi_format(self.dup.format).ok_or_else(|| dxgi_err("pool", "format"))?;
        let (texture, handle) = self.dup.gpu.shared_texture(ID, self.dup.size, format)?;
        let handle = Arc::new(handle);
        self.slots[index] = Some(Slot {
            texture: texture.clone(),
            handle: Arc::clone(&handle),
        });
        Ok((texture, handle))
    }

    fn deliver(
        &mut self,
        frame: &Acquired,
        damage: Damage,
        cursor_mode: CursorMode,
        skipped: &mut u32,
    ) -> Result<Delivery, CaptureError> {
        let cursor = (cursor_mode == CursorMode::Metadata)
            .then(|| self.pointer.state(self.dup.size, self.dup.orientation))
            .flatten();
        let texture = frame.texture()?;
        if self.path == Path::Cpu {
            let image = self.readback.read(ID, &self.dup.gpu, &texture)?;
            let mut meta = frame_meta(
                &self.source,
                frame_facts(
                    &self.dup,
                    BufferMemory::Cpu,
                    image.stride,
                    &frame.info,
                    damage,
                    cursor,
                ),
            );
            meta.tier = self.chosen.tier;
            meta.skipped_before = std::mem::take(skipped);
            return Ok(self.push(meta, NativeBuffer::from_bytes(image.bytes), cursor_mode));
        }
        let slot = (0..u32::from(self.pool_depth))
            .find(|s| !self.pool.is_held(*s))
            .unwrap_or(0);
        let lease = match self.pool.hold(slot) {
            Ok(lease) => lease,
            Err(err @ HoldError::SlotOutOfRange { .. }) => return Err(dxgi_err("pool", err)),
            Err(HoldError::Exhausted(exhausted)) => {
                self.out
                    .exhausted(exhausted, self.pool_depth, self.exhaustion);
                *skipped = skipped.saturating_add(1);
                return match self.exhaustion {
                    // The frame is released at once; its damage goes to
                    // the next one (`owed_damage`).
                    Exhaustion::Stall | Exhaustion::DropNewest => Ok(Delivery::Dropped),
                    Exhaustion::CopyToCpu => {
                        let image = self.readback.read(ID, &self.dup.gpu, &texture)?;
                        let mut meta = frame_meta(
                            &self.source,
                            frame_facts(
                                &self.dup,
                                BufferMemory::Cpu,
                                image.stride,
                                &frame.info,
                                Damage::Full,
                                cursor,
                            ),
                        );
                        meta.tier = self.chosen.tier;
                        meta.copied = Some(CopyStage::ExhaustionReadback);
                        meta.skipped_before = std::mem::take(skipped);
                        Ok(self
                            .out
                            .deliver(meta, NativeBuffer::from_bytes(image.bytes)))
                    }
                };
            }
        };
        let (target, handle) = self.slot(lease.identity().slot)?;
        self.dup.gpu.copy(&target, &texture);
        let finished = self.dup.gpu.try_finish(ID, COPY_WAIT)?;
        if self.late.record(finished) {
            return Err(dxgi_err("GPU copy", "the GPU stopped finishing copies"));
        }
        if !finished {
            // A busy GPU: this frame is dropped, its slot freed.
            drop(lease);
            *skipped = skipped.saturating_add(1);
            return Ok(Delivery::Dropped);
        }
        let mut meta = frame_meta(
            &self.source,
            frame_facts(
                &self.dup,
                BufferMemory::D3d11Texture,
                0,
                &frame.info,
                damage,
                cursor,
            ),
        );
        meta.tier = self.chosen.tier;
        meta.skipped_before = std::mem::take(skipped);
        meta.buffer = lease.identity();
        let identity = lease.identity().into();
        let backing = D3d11Frame::new(
            target,
            self.dup.gpu.device.clone(),
            Some(handle),
            self.dup.gpu.luid,
        );
        let buffer = NativeBuffer::from_platform(backing)
            .with_identity(identity)
            .with_release(move || drop(lease));
        Ok(self.push(meta, buffer, cursor_mode))
    }

    fn push(
        &mut self,
        meta: crate::types::FrameMeta,
        buffer: NativeBuffer,
        cursor_mode: CursorMode,
    ) -> Delivery {
        self.last = (cursor_mode == CursorMode::Metadata).then(|| (meta.clone(), buffer.clone()));
        self.out.deliver(meta, buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_is_turned_back() {
        assert!(orientation_of(DXGI_MODE_ROTATION(1)).is_upright());
        assert_eq!(
            orientation_of(DXGI_MODE_ROTATION_ROTATE90).rotation,
            Rotation::R90
        );
        assert_eq!(
            orientation_of(DXGI_MODE_ROTATION_ROTATE270).rotation,
            Rotation::R270
        );
    }

    #[test]
    fn duplication_failures_are_told_apart() {
        assert_eq!(
            classify(E_ACCESSDENIED),
            DupFailure::NotNow(PauseReason::SecureDesktop)
        );
        assert_eq!(
            classify(DXGI_ERROR_SESSION_DISCONNECTED),
            DupFailure::NotNow(PauseReason::SystemPaused)
        );
        // Never an endless secure-desktop pause.
        assert_eq!(classify(DXGI_ERROR_UNSUPPORTED), DupFailure::Unsupported);
        assert!(matches!(
            dup_error(&DXGI_ERROR_UNSUPPORTED.into()),
            DupError::Busy(_)
        ));
        assert_eq!(classify(E_INVALIDARG), DupFailure::OwnBusy);
        assert_eq!(
            classify(DXGI_ERROR_NOT_CURRENTLY_AVAILABLE),
            DupFailure::OthersBusy
        );
        assert_eq!(classify(DXGI_ERROR_ACCESS_LOST), DupFailure::Fatal);
        let busy = dup_error(&E_INVALIDARG.into());
        assert!(matches!(busy, DupError::Busy(_)));
    }

    #[test]
    fn every_tier_declares_the_mandatory_copy() {
        for offer in offer_list(&formats(true)) {
            assert!(offer.copies.contains(&CopyStage::DxgiMandatory));
        }
    }

    #[test]
    fn the_pointer_hotspot_lands_in_buffer_space() {
        let pointer = Pointer {
            position: Some(Point { x: 10, y: 20 }),
            hotspot: Point { x: 1, y: 2 },
            visible: true,
        };
        let upright = pointer
            .state(Size::new(100, 50), Orientation::UPRIGHT)
            .expect("state");
        assert_eq!(upright.position, Point { x: 11, y: 22 });
        assert!(upright.visible);
    }
}
