//! [`NativeBuffer`]: one platform buffer plus what releases it.

use std::any::Any;
use std::fmt;
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::os::fd::OwnedFd;

use crate::sync::lock;

/// Which kind of memory a [`NativeBuffer`] holds, readable without
/// knowing the backing's concrete type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MemoryKind {
    /// Bytes in this process's address space ([`CpuBuffer`]).
    Cpu,
    /// Linux DMA-BUF planes ([`DmaBuf`]).
    DmaBuf,
    /// Android `AHardwareBuffer`.
    HardwareBuffer,
    /// Apple Core Video `CVPixelBuffer`.
    CvPixelBuffer,
    /// Apple `IOSurface`.
    IoSurface,
    /// Windows `ID3D11Texture2D`.
    D3d11Texture,
    /// Anything else; the producer documents the backing type.
    Other,
}

impl fmt::Display for MemoryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cpu => "CPU memory",
            Self::DmaBuf => "DMA-BUF",
            Self::HardwareBuffer => "AHardwareBuffer",
            Self::CvPixelBuffer => "CVPixelBuffer",
            Self::IoSurface => "IOSurface",
            Self::D3d11Texture => "D3D11 texture",
            Self::Other => "native buffer",
        })
    }
}

type ReleaseHook = Box<dyn FnOnce() + Send>;

struct Inner {
    kind: MemoryKind,
    /// `None` only once [`NativeBuffer::into_backing`] moved it out.
    backing: Option<Box<dyn Any + Send + Sync>>,
    /// Run in order after the backing dropped.
    release: Mutex<Vec<ReleaseHook>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // The backing releases what it owns first (fds, platform
        // references); the hooks then tell a pooled producer the slot is
        // free again.
        drop(self.backing.take());
        let hooks = std::mem::take(self.release.get_mut().unwrap_or_else(|p| p.into_inner()));
        run_hooks(hooks);
    }
}

fn run_hooks(hooks: Vec<ReleaseHook>) {
    for hook in hooks {
        hook();
    }
}

/// A platform buffer whose memory is owned by its producer until the
/// last reference drops.
///
/// The backing is any `Send + Sync` value; its own [`Drop`] releases
/// what it holds (closes fds, drops a platform reference). Hooks added
/// with [`with_release`](Self::with_release) run after it, for pooled
/// producers that reuse the memory.
///
/// Cloning shares the backing: release happens once, when the last
/// clone drops, on whichever thread drops it.
#[derive(Clone)]
pub struct NativeBuffer(Arc<Inner>);

impl NativeBuffer {
    /// Wrap `backing`, a value of `kind` memory.
    #[must_use]
    pub fn new<B: Any + Send + Sync>(kind: MemoryKind, backing: B) -> Self {
        Self(Arc::new(Inner {
            kind,
            backing: Some(Box::new(backing)),
            release: Mutex::new(Vec::new()),
        }))
    }

    /// CPU bytes owned by `data` (a `Vec<u8>`, `Bytes`, a mapping, …),
    /// moved, not copied.
    #[must_use]
    pub fn from_cpu(data: impl AsRef<[u8]> + Send + Sync + 'static) -> Self {
        Self::new(MemoryKind::Cpu, CpuBuffer::new(data))
    }

    /// DMA-BUF planes; the fds close when the buffer is released.
    #[cfg(unix)]
    #[must_use]
    pub fn from_dmabuf(dmabuf: DmaBuf) -> Self {
        Self::new(MemoryKind::DmaBuf, dmabuf)
    }

    /// Add `release`, run once the buffer and every clone dropped (after
    /// the backing itself). Hooks run in the order they were added.
    #[must_use]
    pub fn with_release(self, release: impl FnOnce() + Send + 'static) -> Self {
        lock(&self.0.release).push(Box::new(release));
        self
    }

    /// The kind of memory held.
    #[must_use]
    pub fn kind(&self) -> MemoryKind {
        self.0.kind
    }

    /// The backing as a `B`, when it is one.
    #[must_use]
    pub fn downcast_ref<B: Any>(&self) -> Option<&B> {
        self.0.backing.as_deref()?.downcast_ref()
    }

    /// The bytes of a [`CpuBuffer`] backing.
    #[must_use]
    pub fn cpu_bytes(&self) -> Option<&[u8]> {
        self.downcast_ref::<CpuBuffer>().map(CpuBuffer::as_bytes)
    }

    /// The [`DmaBuf`] backing.
    #[cfg(unix)]
    #[must_use]
    pub fn dmabuf(&self) -> Option<&DmaBuf> {
        self.downcast_ref()
    }

    /// Whether another clone of this buffer exists.
    #[must_use]
    pub fn is_shared(&self) -> bool {
        Arc::strong_count(&self.0) > 1
    }

    /// Move the backing out as a `B`, keeping the release hooks in the
    /// returned [`BufferLease`]: the producer is told the memory is free
    /// only when the lease drops.
    ///
    /// # Errors
    ///
    /// Gives `self` back when it is shared with another clone or its
    /// backing is not a `B`.
    pub fn into_backing<B: Any>(self) -> Result<(B, BufferLease), Self> {
        let mut inner = Arc::try_unwrap(self.0).map_err(Self)?;
        let Some(backing) = inner.backing.take() else {
            return Err(Self(Arc::new(inner)));
        };
        match backing.downcast::<B>() {
            Ok(backing) => {
                let hooks =
                    std::mem::take(inner.release.get_mut().unwrap_or_else(|p| p.into_inner()));
                Ok((*backing, BufferLease { hooks }))
            }
            Err(backing) => {
                inner.backing = Some(backing);
                Err(Self(Arc::new(inner)))
            }
        }
    }
}

impl fmt::Debug for NativeBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeBuffer")
            .field("kind", &self.0.kind)
            .field("refs", &Arc::strong_count(&self.0))
            .finish_non_exhaustive()
    }
}

/// The release hooks of a buffer whose backing was moved out with
/// [`NativeBuffer::into_backing`]. Dropping it tells the producer the
/// memory is free.
#[must_use = "dropping the lease releases the buffer to its producer at once"]
pub struct BufferLease {
    hooks: Vec<ReleaseHook>,
}

impl Drop for BufferLease {
    fn drop(&mut self) {
        run_hooks(std::mem::take(&mut self.hooks));
    }
}

impl fmt::Debug for BufferLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufferLease")
            .field("hooks", &self.hooks.len())
            .finish()
    }
}

/// CPU bytes owned by whatever produced them.
pub struct CpuBuffer {
    data: Box<dyn AsRef<[u8]> + Send + Sync>,
}

impl CpuBuffer {
    /// Bytes owned by `data`, which is kept, not copied.
    #[must_use]
    pub fn new(data: impl AsRef<[u8]> + Send + Sync + 'static) -> Self {
        Self {
            data: Box::new(data),
        }
    }

    /// The bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        (*self.data).as_ref()
    }
}

impl fmt::Debug for CpuBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CpuBuffer")
            .field("len", &self.as_bytes().len())
            .finish()
    }
}

/// One plane of a [`DmaBuf`].
#[cfg(unix)]
#[derive(Debug)]
pub struct DmaBufPlane {
    /// The plane's DMA-BUF; closed when the buffer is released.
    pub fd: OwnedFd,
    /// Byte offset of the plane inside `fd`.
    pub offset: u32,
    /// Row pitch in bytes.
    pub stride: u32,
}

/// A Linux DMA-BUF image: what `linux-dmabuf`, `PipeWire`, GBM, VA-API
/// and Vulkan's external memory exchange.
#[cfg(unix)]
#[derive(Debug)]
pub struct DmaBuf {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// DRM fourcc (`drm_fourcc.h`).
    pub fourcc: u32,
    /// DRM format modifier, shared by every plane.
    pub modifier: u64,
    /// Planes in format order.
    pub planes: Vec<DmaBufPlane>,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Debug, Default)]
    struct Probe(Arc<AtomicUsize>);

    impl Drop for Probe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn release_runs_after_the_backing_and_only_once() {
        struct Backing(Arc<Mutex<Vec<&'static str>>>);
        impl Drop for Backing {
            fn drop(&mut self) {
                lock(&self.0).push("backing");
            }
        }
        let order = Arc::new(Mutex::new(Vec::new()));
        let hook_order = Arc::clone(&order);
        let buffer = NativeBuffer::new(MemoryKind::Other, Backing(Arc::clone(&order)))
            .with_release(move || lock(&hook_order).push("hook"));
        let clone = buffer.clone();
        drop(buffer);
        assert!(lock(&order).is_empty(), "a clone still holds it");
        drop(clone);
        assert_eq!(*lock(&order), ["backing", "hook"]);
    }

    #[test]
    fn into_backing_moves_the_value_and_keeps_the_hooks_in_the_lease() {
        let released = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&released);
        let buffer = NativeBuffer::from_cpu(vec![1u8, 2, 3]).with_release(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });

        let shared = buffer.clone();
        let buffer = buffer
            .into_backing::<CpuBuffer>()
            .expect_err("shared buffers keep their backing");
        drop(shared);

        let buffer = buffer
            .into_backing::<Probe>()
            .expect_err("wrong type gives the buffer back");
        assert_eq!(buffer.cpu_bytes(), Some(&[1u8, 2, 3][..]));

        let (cpu, lease) = buffer
            .into_backing::<CpuBuffer>()
            .expect("unique, right type");
        assert_eq!(cpu.as_bytes(), [1, 2, 3]);
        assert_eq!(released.load(Ordering::SeqCst), 0);
        drop(lease);
        assert_eq!(released.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn downcast_and_kind() {
        let drops = Arc::new(AtomicUsize::new(0));
        let buffer = NativeBuffer::new(MemoryKind::HardwareBuffer, Probe(Arc::clone(&drops)));
        assert_eq!(buffer.kind(), MemoryKind::HardwareBuffer);
        assert!(buffer.downcast_ref::<Probe>().is_some());
        assert!(buffer.cpu_bytes().is_none());
        drop(buffer);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dmabuf_fds_close_on_release() {
        use std::os::fd::AsRawFd;

        let file = std::fs::File::open("/dev/null").expect("open /dev/null");
        let fd: OwnedFd = file.into();
        let raw = fd.as_raw_fd();
        let buffer = NativeBuffer::from_dmabuf(DmaBuf {
            width: 4,
            height: 4,
            fourcc: u32::from_le_bytes(*b"XR24"),
            modifier: 0,
            planes: vec![DmaBufPlane {
                fd,
                offset: 0,
                stride: 16,
            }],
        });
        assert_eq!(buffer.dmabuf().map(|d| d.planes.len()), Some(1));
        drop(buffer);
        // The descriptor number is free again (or reused by someone
        // else, which `fcntl` cannot tell apart); `/proc` says which.
        let link = std::fs::read_link(format!("/proc/self/fd/{raw}"));
        assert!(link.map_or(true, |path| path != std::path::Path::new("/dev/null")));
    }
}
