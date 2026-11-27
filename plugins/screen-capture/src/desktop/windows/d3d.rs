//! Direct3D 11 / DXGI plumbing shared by the GPU backends: adapters by
//! LUID or preference, a device per session, staging readbacks into CPU
//! memory (`CopyStage::GpuReadback`), shareable NT-handle textures and
//! waiting for the GPU to finish a copy.

#![allow(
    unsafe_code,
    reason = "Direct3D 11 / DXGI COM calls are unsafe in the windows crate"
)]

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{GENERIC_ALL, HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_QUERY_DESC, D3D11_QUERY_EVENT,
    D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING, D3D11CreateDevice,
    ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Query, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R10G10B10A2_UNORM,
    DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE, DXGI_GPU_PREFERENCE_MINIMUM_POWER,
    IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, IDXGIFactory6, IDXGIOutput,
    IDXGIResource1,
};
use windows::core::Interface as _;

use crate::d3d11::SharedHandle;
use crate::types::{BackendId, CaptureError, GpuPreference, PixelFormat, Size};

/// A LUID as the `u64` `GpuIdentity::Luid` carries (high part first).
#[must_use]
pub(crate) fn luid_u64(luid: LUID) -> u64 {
    (u64::from(luid.HighPart.cast_unsigned()) << 32) | u64::from(luid.LowPart)
}

/// The plugin's pixel format for a DXGI format it delivers.
#[must_use]
pub(crate) const fn pixel_format(format: DXGI_FORMAT) -> Option<PixelFormat> {
    match format {
        DXGI_FORMAT_B8G8R8A8_UNORM => Some(PixelFormat::Bgra8),
        DXGI_FORMAT_R16G16B16A16_FLOAT => Some(PixelFormat::Rgba16f),
        DXGI_FORMAT_R10G10B10A2_UNORM => Some(PixelFormat::Xb30),
        _ => None,
    }
}

/// The DXGI format of a pixel format the GPU backends allocate.
#[must_use]
pub(crate) const fn dxgi_format(format: PixelFormat) -> Option<DXGI_FORMAT> {
    match format {
        PixelFormat::Bgra8 | PixelFormat::Bgrx8 => Some(DXGI_FORMAT_B8G8R8A8_UNORM),
        PixelFormat::Rgba16f => Some(DXGI_FORMAT_R16G16B16A16_FLOAT),
        PixelFormat::Xb30 => Some(DXGI_FORMAT_R10G10B10A2_UNORM),
        _ => None,
    }
}

fn hr(backend: BackendId, operation: &str, err: &windows_core::Error) -> CaptureError {
    CaptureError::os(backend, operation, i64::from(err.code().0))
}

/// One adapter, by LUID.
#[derive(Debug, Clone)]
pub(crate) struct Adapter {
    pub(crate) dxgi: IDXGIAdapter1,
    pub(crate) luid: u64,
    /// Software adapters (WARP, "Microsoft Basic Render Driver").
    pub(crate) software: bool,
}

fn factory(backend: BackendId) -> Result<IDXGIFactory1, CaptureError> {
    // SAFETY: CreateDXGIFactory1 has no preconditions.
    unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }
        .map_err(|e| hr(backend, "CreateDXGIFactory1", &e))
}

fn describe(adapter: IDXGIAdapter1) -> Option<Adapter> {
    // SAFETY: `adapter` is a live adapter.
    let desc = unsafe { adapter.GetDesc1() }.ok()?;
    // DXGI_ADAPTER_FLAG_SOFTWARE.
    let software = desc.Flags & 2 != 0;
    Some(Adapter {
        dxgi: adapter,
        luid: luid_u64(desc.AdapterLuid),
        software,
    })
}

/// Every adapter, in DXGI's order (the first drives the primary
/// monitor).
pub(crate) fn adapters(backend: BackendId) -> Result<Vec<Adapter>, CaptureError> {
    let factory = factory(backend)?;
    let mut adapters = Vec::new();
    for index in 0.. {
        // SAFETY: `factory` is live; an index past the end returns
        // DXGI_ERROR_NOT_FOUND.
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(index) }) else {
            break;
        };
        adapters.extend(describe(adapter));
    }
    Ok(adapters)
}

/// The adapter `preference` names, `None` for "the system's default".
///
/// # Errors
/// `Unsatisfiable(Gpu)` when `Same(luid)` names no adapter here.
pub(crate) fn adapter_for(
    backend: BackendId,
    preference: GpuPreference,
) -> Result<Option<Adapter>, CaptureError> {
    use crate::types::GpuIdentity;
    let by_preference = |high: bool| -> Result<Option<Adapter>, CaptureError> {
        let factory = factory(backend)?;
        let Ok(factory) = factory.cast::<IDXGIFactory6>() else {
            // Before Windows 10 1803: no preference query, the default.
            return Ok(None);
        };
        let preference = if high {
            DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE
        } else {
            DXGI_GPU_PREFERENCE_MINIMUM_POWER
        };
        // SAFETY: `factory` is live; index 0 always exists when any
        // adapter does.
        let adapter = unsafe { factory.EnumAdapterByGpuPreference::<IDXGIAdapter1>(0, preference) };
        Ok(adapter.ok().and_then(describe))
    };
    match preference {
        GpuPreference::Auto => Ok(None),
        GpuPreference::Discrete => by_preference(true),
        GpuPreference::Integrated => by_preference(false),
        GpuPreference::Same(GpuIdentity::Luid(luid)) => adapters(backend)?
            .into_iter()
            .find(|a| a.luid == luid)
            .map(Some)
            .ok_or_else(|| {
                CaptureError::unsatisfiable(Some(backend), crate::types::Requirement::Gpu)
            }),
        GpuPreference::Same(_) => Err(CaptureError::unsatisfiable(
            Some(backend),
            crate::types::Requirement::Gpu,
        )),
    }
}

/// The adapter and output driving the monitor whose GDI device name is
/// `device` (`\\.\DISPLAY1`).
pub(crate) fn output_for(
    backend: BackendId,
    device: &str,
) -> Result<Option<(Adapter, IDXGIOutput)>, CaptureError> {
    for adapter in adapters(backend)? {
        for index in 0.. {
            // SAFETY: `adapter` is live; an index past the end returns
            // DXGI_ERROR_NOT_FOUND.
            let Ok(output) = (unsafe { adapter.dxgi.EnumOutputs(index) }) else {
                break;
            };
            // SAFETY: `output` is live.
            let Ok(desc) = (unsafe { output.GetDesc() }) else {
                continue;
            };
            if super::wide_to_string(&desc.DeviceName) == device {
                return Ok(Some((adapter, output)));
            }
        }
    }
    Ok(None)
}

/// A D3D11 device and its immediate context, multithread-protected so
/// a consumer may use the device (and its immediate context) from its
/// own threads while the capture thread copies.
#[derive(Debug, Clone)]
pub(crate) struct Gpu {
    pub(crate) device: ID3D11Device,
    pub(crate) context: ID3D11DeviceContext,
    pub(crate) luid: u64,
}

impl Gpu {
    /// A hardware device on `adapter`, or on the default adapter.
    pub(crate) fn new(backend: BackendId, adapter: Option<&Adapter>) -> Result<Self, CaptureError> {
        let mut device = None;
        let mut context = None;
        let driver = if adapter.is_some() {
            D3D_DRIVER_TYPE_UNKNOWN
        } else {
            D3D_DRIVER_TYPE_HARDWARE
        };
        let dxgi: Option<IDXGIAdapter> = adapter.and_then(|a| a.dxgi.cast().ok());
        // SAFETY: the out pointers are live locals; a null adapter with
        // the hardware driver type picks the default adapter.
        let created = unsafe {
            D3D11CreateDevice(
                dxgi.as_ref(),
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&raw mut device),
                None,
                Some(&raw mut context),
            )
        };
        created.map_err(|e| hr(backend, "D3D11CreateDevice", &e))?;
        let (Some(device), Some(context)) = (device, context) else {
            return Err(CaptureError::protocol(
                backend,
                "D3D11CreateDevice",
                "no device returned",
            ));
        };
        if let Ok(multithread) = context.cast::<ID3D11Multithread>() {
            // SAFETY: `multithread` is the live context's interface.
            let _was = unsafe { multithread.SetMultithreadProtected(true) };
        }
        let luid = device_luid(&device).unwrap_or(0);
        Ok(Self {
            device,
            context,
            luid,
        })
    }

    /// Why the driver removed the device (TDR, driver update, GPU
    /// switch), once it did: every later call on it fails.
    pub(crate) fn removed(&self, backend: BackendId) -> Option<CaptureError> {
        // SAFETY: a live device; the call only reads its state.
        let reason = unsafe { self.device.GetDeviceRemovedReason() };
        reason
            .err()
            .map(|e| hr(backend, "ID3D11Device.GetDeviceRemovedReason", &e))
    }

    /// `err` from a call on this device, or the removal that caused it.
    pub(crate) fn failure(
        &self,
        backend: BackendId,
        operation: &str,
        err: &windows_core::Error,
    ) -> CaptureError {
        self.removed(backend)
            .unwrap_or_else(|| hr(backend, operation, err))
    }

    /// A texture like `like` (same size and format) for `usage`.
    fn texture(
        &self,
        backend: BackendId,
        size: Size,
        format: DXGI_FORMAT,
        kind: TextureKind,
    ) -> Result<ID3D11Texture2D, CaptureError> {
        let (usage, bind, cpu, misc) = match kind {
            TextureKind::Staging => (D3D11_USAGE_STAGING, 0, D3D11_CPU_ACCESS_READ.0, 0),
            TextureKind::Shared => (
                D3D11_USAGE_DEFAULT,
                D3D11_BIND_SHADER_RESOURCE.0,
                0,
                D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0,
            ),
        };
        let desc = D3D11_TEXTURE2D_DESC {
            Width: size.width,
            Height: size.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: usage,
            BindFlags: bind.cast_unsigned(),
            CPUAccessFlags: cpu.cast_unsigned(),
            MiscFlags: misc.cast_unsigned(),
        };
        let mut texture = None;
        // SAFETY: `desc` and the out pointer are live locals.
        let created = unsafe {
            self.device
                .CreateTexture2D(&raw const desc, None, Some(&raw mut texture))
        };
        created.map_err(|e| hr(backend, "CreateTexture2D", &e))?;
        texture.ok_or_else(|| CaptureError::protocol(backend, "CreateTexture2D", "no texture"))
    }

    /// A texture other devices (and processes) can open through its NT
    /// handle.
    pub(crate) fn shared_texture(
        &self,
        backend: BackendId,
        size: Size,
        format: DXGI_FORMAT,
    ) -> Result<(ID3D11Texture2D, SharedHandle), CaptureError> {
        let texture = self.texture(backend, size, format, TextureKind::Shared)?;
        let resource = texture
            .cast::<IDXGIResource1>()
            .map_err(|e| hr(backend, "IDXGIResource1", &e))?;
        // SAFETY: `resource` was created with SHARED_NTHANDLE; no security
        // attributes (the handle is not inheritable), no name.
        let handle = unsafe {
            resource.CreateSharedHandle(None, GENERIC_ALL.0, windows_core::PCWSTR::null())
        }
        .map_err(|e| hr(backend, "CreateSharedHandle", &e))?;
        Ok((texture, SharedHandle::from_raw(handle)))
    }

    /// Copy all of `src` into `dst` (same size and format).
    pub(crate) fn copy(&self, dst: &ID3D11Texture2D, src: &ID3D11Texture2D) {
        // SAFETY: both textures are live resources of this device with the
        // same size and format; the context is multithread-protected.
        unsafe { self.context.CopyResource(dst, src) };
    }

    /// Submit the queued work and wait (at most `timeout`) until the GPU
    /// finished it, so a frame leaves as `SyncKind::Ready`.
    pub(crate) fn finish(&self, backend: BackendId, timeout: Duration) -> Result<(), CaptureError> {
        let desc = D3D11_QUERY_DESC {
            Query: D3D11_QUERY_EVENT,
            MiscFlags: 0,
        };
        let mut query: Option<ID3D11Query> = None;
        // SAFETY: `desc` and the out pointer are live locals.
        let created = unsafe {
            self.device
                .CreateQuery(&raw const desc, Some(&raw mut query))
        };
        created.map_err(|e| hr(backend, "CreateQuery", &e))?;
        let query =
            query.ok_or_else(|| CaptureError::protocol(backend, "CreateQuery", "no query"))?;
        // SAFETY: `query` is a live event query of this device.
        unsafe { self.context.End(&query) };
        // SAFETY: submits the context's queued commands.
        unsafe { self.context.Flush() };
        let deadline = Instant::now() + timeout;
        loop {
            let mut done = windows_core::BOOL(0);
            // SAFETY: an event query writes one BOOL into `done`.
            let polled = unsafe {
                self.context.GetData(
                    &query,
                    Some((&raw mut done).cast()),
                    u32::try_from(std::mem::size_of::<windows_core::BOOL>()).unwrap_or(4),
                    0,
                )
            };
            // S_FALSE (not finished yet) arrives as `Ok` with `done` false.
            if polled.is_ok() && done.as_bool() {
                return Ok(());
            }
            if let Err(err) = polled {
                return Err(self.failure(backend, "GetData", &err));
            }
            if Instant::now() >= deadline {
                return Err(CaptureError::protocol(
                    backend,
                    "GPU copy",
                    "the copy did not finish in time",
                ));
            }
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextureKind {
    Staging,
    Shared,
}

fn device_luid(device: &ID3D11Device) -> Option<u64> {
    let dxgi = device.cast::<IDXGIDevice>().ok()?;
    // SAFETY: `dxgi` is the live device's DXGI interface.
    let adapter = unsafe { dxgi.GetAdapter() }.ok()?;
    // SAFETY: `adapter` is live.
    let desc = unsafe { adapter.GetDesc() }.ok()?;
    Some(luid_u64(desc.AdapterLuid))
}

/// The size and DXGI format of a texture.
pub(crate) fn texture_desc(texture: &ID3D11Texture2D) -> D3D11_TEXTURE2D_DESC {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: `desc` is a live, writable descriptor.
    unsafe { texture.GetDesc(&raw mut desc) };
    desc
}

/// Pixels read back into CPU memory, rows tight.
#[derive(Debug)]
pub(crate) struct CpuImage {
    pub(crate) bytes: Vec<u8>,
    pub(crate) stride: u32,
}

/// A reused staging texture for readbacks (`CopyStage::GpuReadback`).
#[derive(Debug, Default)]
pub(crate) struct Readback {
    staging: Option<(ID3D11Texture2D, Size, DXGI_FORMAT)>,
}

impl Readback {
    /// Copy `src` into CPU memory: GPU copy into the staging texture,
    /// map, rows copied out, unmap.
    pub(crate) fn read(
        &mut self,
        backend: BackendId,
        gpu: &Gpu,
        src: &ID3D11Texture2D,
    ) -> Result<CpuImage, CaptureError> {
        let desc = texture_desc(src);
        let size = Size::new(desc.Width, desc.Height);
        let format = pixel_format(desc.Format).ok_or_else(|| {
            CaptureError::protocol(
                backend,
                "readback",
                format!("unknown DXGI format {}", desc.Format.0),
            )
        })?;
        let staging = match &self.staging {
            Some((texture, s, f)) if *s == size && *f == desc.Format => texture.clone(),
            _ => {
                let texture = gpu.texture(backend, size, desc.Format, TextureKind::Staging)?;
                self.staging = Some((texture.clone(), size, desc.Format));
                texture
            }
        };
        gpu.copy(&staging, src);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: `staging` is a CPU-readable staging texture of this
        // device; `mapped` is a live out structure. Map waits for the copy.
        let map = unsafe {
            gpu.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped))
        };
        map.map_err(|e| gpu.failure(backend, "Map", &e))?;
        let row = usize::try_from(size.width * format.bytes_per_pixel()).unwrap_or(0);
        let rows = usize::try_from(size.height).unwrap_or(0);
        let pitch = usize::try_from(mapped.RowPitch).unwrap_or(0);
        let mut bytes = Vec::new();
        let copied = if mapped.pData.is_null() || pitch < row || rows == 0 {
            Err(CaptureError::protocol(backend, "Map", "bad mapping"))
        } else if bytes.try_reserve_exact(row * rows).is_err() {
            Err(CaptureError::protocol(backend, "readback", "out of memory"))
        } else {
            // SAFETY: a mapped subresource of `rows` rows `pitch` apart,
            // each at least `row` bytes long, valid until Unmap below.
            let mapping = unsafe {
                std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * (rows - 1) + row)
            };
            for y in 0..rows {
                bytes.extend_from_slice(&mapping[y * pitch..y * pitch + row]);
            }
            Ok(())
        };
        // SAFETY: unmaps the subresource mapped above; `mapping` is gone.
        unsafe { gpu.context.Unmap(&staging, 0) };
        copied?;
        Ok(CpuImage {
            bytes,
            stride: u32::try_from(row).unwrap_or(0),
        })
    }

    /// Forget the staging texture (a new device or size).
    pub(crate) fn reset(&mut self) {
        self.staging = None;
    }
}
