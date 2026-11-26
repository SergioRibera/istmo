//! The D3D11 frame backing of the Windows GPU backends.
//!
//! A frame with `BufferMemory::D3d11Texture` carries a [`D3d11Frame`]:
//! read it with `buffer.downcast_ref::<D3d11Frame>()`, or take it with
//! `into_backing::<D3d11Frame>()` (the lease then holds the pool slot).
//! `NativeBuffer::platform_ptr` is the `ID3D11Texture2D*`.
//!
//! - **Zero-copy frames** (no copy in `NegotiatedStream::copies`) are
//!   the capture pool's own textures, on the plugin's device
//!   ([`device`](D3d11Frame::device)). The device is
//!   multithread-protected: a consumer may use it (and its immediate
//!   context) from its own threads. These textures are not shareable,
//!   so [`shared_handle`](D3d11Frame::shared_handle) is `None`.
//! - **Shareable frames** (`CopyStage::CrossDevice`, and every DXGI
//!   frame) are plugin-owned textures created with
//!   `D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_NTHANDLE`:
//!   open them on another device with `ID3D11Device1::OpenSharedResource1`
//!   and the NT handle.
//!
//! Sync: no fence or keyed mutex travels with a frame. The plugin
//! pushes a frame once the GPU finished writing it (`SyncKind::Ready`);
//! dropping the frame (and its lease) means every read of it, GPU work
//! included, is complete, and only then may the texture be written again.

#![allow(
    unsafe_code,
    reason = "closing the NT handle of a shared texture (CloseHandle)"
)]

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use istmo_core::{MemoryKind, PlatformBuffer};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::core::Interface as _;

/// An owned NT handle to a shared texture, closed on drop.
#[derive(Debug)]
pub(crate) struct SharedHandle(usize);

impl SharedHandle {
    /// Own `handle` (from `IDXGIResource1::CreateSharedHandle`).
    pub(crate) fn from_raw(handle: HANDLE) -> Self {
        Self(handle.0 as usize)
    }

    const fn handle(&self) -> HANDLE {
        HANDLE(self.0 as *mut c_void)
    }
}

impl Drop for SharedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by CreateSharedHandle, is owned
        // by this value alone and is closed exactly once, here.
        if let Err(err) = unsafe { CloseHandle(self.handle()) } {
            tracing::warn!(%err, "screen-capture: closing a shared texture handle failed");
        }
    }
}

/// A captured frame in a D3D11 texture.
#[derive(Debug, Clone)]
pub struct D3d11Frame {
    texture: ID3D11Texture2D,
    device: ID3D11Device,
    shared: Option<Arc<SharedHandle>>,
    adapter_luid: u64,
}

impl D3d11Frame {
    pub(crate) const fn new(
        texture: ID3D11Texture2D,
        device: ID3D11Device,
        shared: Option<Arc<SharedHandle>>,
        adapter_luid: u64,
    ) -> Self {
        Self {
            texture,
            device,
            shared,
            adapter_luid,
        }
    }

    /// The texture. Read it only; it goes back to the pool when the
    /// frame drops.
    #[must_use]
    pub const fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }

    /// The device the texture belongs to.
    #[must_use]
    pub const fn device(&self) -> &ID3D11Device {
        &self.device
    }

    /// The texture's NT handle, for `OpenSharedResource1` on another
    /// device or (duplicated) in another process. Valid while the frame
    /// lives; `None` for the capture pool's own, unshareable textures.
    #[must_use]
    pub fn shared_handle(&self) -> Option<HANDLE> {
        self.shared.as_ref().map(|h| h.handle())
    }

    /// The LUID of the adapter that holds the texture (as
    /// `GpuIdentity::Luid`).
    #[must_use]
    pub const fn adapter_luid(&self) -> u64 {
        self.adapter_luid
    }
}

impl PlatformBuffer for D3d11Frame {
    fn kind(&self) -> MemoryKind {
        MemoryKind::D3d11Texture
    }

    fn as_ptr(&self) -> NonNull<c_void> {
        // A live COM interface pointer is never null.
        NonNull::new(self.texture.as_raw()).unwrap_or(NonNull::dangling())
    }
}
