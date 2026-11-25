//! [`NativeBuffer`]: one platform buffer plus what releases it.

use std::any::Any;
use std::ffi::c_void;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

#[cfg(unix)]
use std::os::fd::OwnedFd;

use crate::sync::lock;

/// Which kind of memory a [`NativeBuffer`] holds, readable without
/// knowing the backing's concrete type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MemoryKind {
    /// Bytes in this process's address space
    /// ([`NativeBuffer::from_bytes`]).
    Cpu,
    /// Linux DMA-BUF planes ([`DmaBuf`]).
    DmaBuf,
    /// Android `AHardwareBuffer`.
    HardwareBuffer,
    /// Apple Core Video `CVPixelBuffer`.
    CvPixelBuffer,
    /// Apple `IOSurface`.
    IoSurface,
    /// Apple Core Media `CMSampleBuffer`.
    CmSampleBuffer,
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
            Self::CmSampleBuffer => "CMSampleBuffer",
            Self::D3d11Texture => "D3D11 texture",
            Self::Other => "native buffer",
        })
    }
}

/// A platform object a backing holds a reference to: an
/// `AHardwareBuffer*`, `CVPixelBufferRef`, `IOSurfaceRef`,
/// `CMSampleBufferRef`, `ID3D11Texture2D*`, …
///
/// Whoever produced the buffer (a transport crate, a desktop plugin),
/// consumers reach the object the same way:
/// [`NativeBuffer::platform_ptr`] plus [`NativeBuffer::kind`].
pub trait PlatformBuffer: Send + Sync + 'static {
    /// What the object is.
    fn kind(&self) -> MemoryKind;

    /// The object, valid while `self` lives. Never null.
    fn as_ptr(&self) -> NonNull<c_void>;
}

/// Where a pooled buffer lives, so consumers can cache what they build
/// from it (an `EGLImage`, a `VkImage`, an imported VA surface) instead
/// of re-importing every frame.
///
/// Two buffers with equal identities are the same memory. A producer
/// takes a new [`pool_generation`](Self::pool_generation) (from
/// [`next_generation`](Self::next_generation)) whenever it reallocates
/// its pool, so a cached import keyed on an identity is never stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferIdentity {
    /// Process-unique id of the pool allocation the buffer belongs to.
    pub pool_generation: u64,
    /// The buffer's slot in that pool.
    pub slot: u32,
}

impl BufferIdentity {
    /// Slot `slot` of pool generation `pool_generation`.
    #[must_use]
    pub const fn new(pool_generation: u64, slot: u32) -> Self {
        Self {
            pool_generation,
            slot,
        }
    }

    /// A pool generation no other pool in this process uses.
    #[must_use]
    pub fn next_generation() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }
}

type ReleaseHook = Box<dyn FnOnce() + Send>;

/// Release hooks, in the order they were added. The first one lives
/// inline: a buffer with one hook allocates nothing for the list.
#[derive(Default)]
struct Hooks {
    first: Option<ReleaseHook>,
    more: Vec<ReleaseHook>,
}

impl Hooks {
    fn push(&mut self, hook: ReleaseHook) {
        if self.first.is_none() {
            self.first = Some(hook);
        } else {
            self.more.push(hook);
        }
    }

    fn len(&self) -> usize {
        usize::from(self.first.is_some()) + self.more.len()
    }

    /// Run every hook; one that panics is logged and the rest still run.
    fn run(self) {
        for hook in self.first.into_iter().chain(self.more) {
            if catch_unwind(AssertUnwindSafe(hook)).is_err() {
                tracing::error!("a native buffer release hook panicked; running the others");
            }
        }
    }
}

/// How a holder reads its backing without knowing its type.
struct View<B> {
    cpu: Option<fn(&B) -> &[u8]>,
    platform: Option<fn(&B) -> NonNull<c_void>>,
}

impl<B> View<B> {
    const NONE: Self = Self {
        cpu: None,
        platform: None,
    };
}

/// What [`NativeBuffer`] needs from its holder without knowing the
/// backing type.
trait Backing: Any + Send + Sync {
    fn kind(&self) -> MemoryKind;
    fn backing_any(&self) -> Option<&dyn Any>;
    fn as_bytes(&self) -> Option<&[u8]>;
    fn platform_ptr(&self) -> Option<NonNull<c_void>>;
    fn identity(&self) -> Option<BufferIdentity>;
    fn set_identity(&self, identity: BufferIdentity);
    fn add_release(&self, hook: ReleaseHook);
    fn add_release_mut(&mut self, hook: ReleaseHook);
    /// Move the backing into `out` (an `Option<B>`) and the hooks into a
    /// lease, when this is the last reference and `out` fits.
    fn take_into(self: Arc<Self>, out: &mut dyn Any) -> Result<BufferLease, TakeError>;
}

enum TakeError {
    WrongType(Arc<dyn Backing>),
    Shared(Arc<dyn Backing>),
}

/// The one allocation behind a [`NativeBuffer`]: the backing plus its
/// release hooks.
struct Holder<B> {
    kind: MemoryKind,
    /// `None` only once [`NativeBuffer::into_backing`] moved it out.
    backing: Option<B>,
    view: View<B>,
    identity: OnceLock<BufferIdentity>,
    release: Mutex<Hooks>,
}

impl<B> Holder<B> {
    fn take_hooks(&mut self) -> Hooks {
        std::mem::take(self.release.get_mut().unwrap_or_else(|p| p.into_inner()))
    }
}

impl<B> Drop for Holder<B> {
    fn drop(&mut self) {
        // The backing releases what it owns first (fds, platform
        // references); the hooks then tell a pooled producer the slot is
        // free again. Neither may unwind into whoever dropped the last
        // reference: a producer evicting a frame, a consumer, a JNI or C
        // entry point.
        if let Some(backing) = self.backing.take()
            && catch_unwind(AssertUnwindSafe(move || drop(backing))).is_err()
        {
            tracing::error!(kind = %self.kind, "a native buffer backing panicked on drop");
        }
        self.take_hooks().run();
    }
}

impl<B: Any + Send + Sync> Backing for Holder<B> {
    fn kind(&self) -> MemoryKind {
        self.kind
    }

    fn backing_any(&self) -> Option<&dyn Any> {
        self.backing.as_ref().map(|b| b as &dyn Any)
    }

    fn as_bytes(&self) -> Option<&[u8]> {
        self.view
            .cpu
            .zip(self.backing.as_ref())
            .map(|(view, backing)| view(backing))
    }

    fn platform_ptr(&self) -> Option<NonNull<c_void>> {
        self.view
            .platform
            .zip(self.backing.as_ref())
            .map(|(view, backing)| view(backing))
    }

    fn identity(&self) -> Option<BufferIdentity> {
        self.identity.get().copied()
    }

    fn set_identity(&self, identity: BufferIdentity) {
        let _already_set = self.identity.set(identity);
    }

    fn add_release(&self, hook: ReleaseHook) {
        lock(&self.release).push(hook);
    }

    fn add_release_mut(&mut self, hook: ReleaseHook) {
        self.release
            .get_mut()
            .unwrap_or_else(|p| p.into_inner())
            .push(hook);
    }

    fn take_into(self: Arc<Self>, out: &mut dyn Any) -> Result<BufferLease, TakeError> {
        let Some(slot) = out.downcast_mut::<Option<B>>() else {
            return Err(TakeError::WrongType(self));
        };
        let mut holder = Arc::try_unwrap(self).map_err(|shared| TakeError::Shared(shared))?;
        *slot = holder.backing.take();
        Ok(BufferLease {
            hooks: Mutex::new(holder.take_hooks()),
        })
    }
}

/// A platform buffer whose memory is owned by its producer until the
/// last reference drops.
///
/// The backing is any `Send + Sync` value; its own [`Drop`] releases
/// what it holds (closes fds, drops a platform reference, returns a
/// pool slot). Hooks added with [`with_release`](Self::with_release)
/// run after it. Neither may panic out: a panic is logged and the other
/// releases still run.
///
/// Cloning shares the backing: release happens once, when the last
/// clone drops, on whichever thread drops it. One allocation per
/// buffer, whatever the backing; a release hook adds the allocation of
/// its closure (a backing whose `Drop` does the release adds none).
#[derive(Clone)]
pub struct NativeBuffer(Arc<dyn Backing>);

impl NativeBuffer {
    fn holder<B: Any + Send + Sync>(kind: MemoryKind, backing: B, view: View<B>) -> Self {
        Self(Arc::new(Holder {
            kind,
            backing: Some(backing),
            view,
            identity: OnceLock::new(),
            release: Mutex::new(Hooks::default()),
        }))
    }

    /// Wrap `backing`, a value of `kind` memory.
    #[must_use]
    pub fn new<B: Any + Send + Sync>(kind: MemoryKind, backing: B) -> Self {
        Self::holder(kind, backing, View::NONE)
    }

    /// Bytes owned by `data` (a `Vec<u8>`, `Bytes`, a mapping, a pool
    /// slot, …), kept as the backing: moved, not copied.
    #[must_use]
    pub fn from_bytes<T: AsRef<[u8]> + Send + Sync + 'static>(data: T) -> Self {
        Self::holder(
            MemoryKind::Cpu,
            data,
            View {
                cpu: Some(<T as AsRef<[u8]>>::as_ref),
                platform: None,
            },
        )
    }

    /// A platform object; [`platform_ptr`](Self::platform_ptr) reads it
    /// back whoever made it.
    #[must_use]
    pub fn from_platform<P: PlatformBuffer>(object: P) -> Self {
        let kind = object.kind();
        Self::holder(
            kind,
            object,
            View {
                cpu: None,
                platform: Some(<P as PlatformBuffer>::as_ptr),
            },
        )
    }

    /// DMA-BUF planes, closed when the last [`Arc`] of them drops.
    ///
    /// A pooled producer keeps an `Arc<DmaBuf>` per slot and passes a
    /// clone: no `dup` per frame, and the fd numbers a consumer sees stay
    /// the same for a slot. Every plane fd must be close-on-exec
    /// (`O_CLOEXEC` / `DRM_CLOEXEC`); debug builds check it.
    #[cfg(unix)]
    #[must_use]
    pub fn from_dmabuf(dmabuf: impl Into<Arc<DmaBuf>>) -> Self {
        let dmabuf = dmabuf.into();
        debug_assert!(
            dmabuf.planes.iter().all(DmaBufPlane::is_cloexec),
            "DMA-BUF plane fds must be close-on-exec"
        );
        Self::new(MemoryKind::DmaBuf, dmabuf)
    }

    /// Add `release`, run once the buffer and every clone dropped (after
    /// the backing itself). Hooks run in the order they were added.
    #[must_use]
    pub fn with_release(mut self, release: impl FnOnce() + Send + 'static) -> Self {
        let hook: ReleaseHook = Box::new(release);
        match Arc::get_mut(&mut self.0) {
            Some(holder) => holder.add_release_mut(hook),
            None => self.0.add_release(hook),
        }
        self
    }

    /// Tag the buffer with where it lives in its producer's pool. The
    /// first identity set sticks.
    #[must_use]
    pub fn with_identity(self, identity: BufferIdentity) -> Self {
        self.0.set_identity(identity);
        self
    }

    /// Where the buffer lives in its producer's pool, when the producer
    /// said.
    #[must_use]
    pub fn identity(&self) -> Option<BufferIdentity> {
        self.0.identity()
    }

    /// The kind of memory held.
    #[must_use]
    pub fn kind(&self) -> MemoryKind {
        self.0.kind()
    }

    /// The backing as a `B`, when it is one.
    #[must_use]
    pub fn downcast_ref<B: Any>(&self) -> Option<&B> {
        self.0.backing_any()?.downcast_ref()
    }

    /// The bytes of a buffer made with [`from_bytes`](Self::from_bytes).
    #[must_use]
    pub fn as_bytes(&self) -> Option<&[u8]> {
        self.0.as_bytes()
    }

    /// The platform object of a buffer made with
    /// [`from_platform`](Self::from_platform), valid while the buffer
    /// lives; [`kind`](Self::kind) says what it is.
    #[must_use]
    pub fn platform_ptr(&self) -> Option<NonNull<c_void>> {
        self.0.platform_ptr()
    }

    /// The [`DmaBuf`] backing.
    #[cfg(unix)]
    #[must_use]
    pub fn dmabuf(&self) -> Option<&DmaBuf> {
        self.downcast_ref::<Arc<DmaBuf>>().map(|dmabuf| &**dmabuf)
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
    /// Gives `self` back in [`IntoBackingError`] when it is shared with
    /// another clone or its backing is not a `B`.
    pub fn into_backing<B: Any + Send + Sync>(self) -> Result<(B, BufferLease), IntoBackingError> {
        let mut out: Option<B> = None;
        match self.0.take_into(&mut out) {
            Ok(lease) => {
                let backing = out.expect("invariant: take_into fills `out` when it succeeds");
                Ok((backing, lease))
            }
            Err(TakeError::WrongType(holder)) => Err(IntoBackingError::WrongType(Self(holder))),
            Err(TakeError::Shared(holder)) => Err(IntoBackingError::Shared(Self(holder))),
        }
    }
}

impl fmt::Debug for NativeBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeBuffer")
            .field("kind", &self.0.kind())
            .field("identity", &self.0.identity())
            .field("refs", &Arc::strong_count(&self.0))
            .finish_non_exhaustive()
    }
}

/// Why [`NativeBuffer::into_backing`] gave the buffer back.
#[derive(Debug)]
#[non_exhaustive]
pub enum IntoBackingError {
    /// Another clone of the buffer is alive.
    Shared(NativeBuffer),
    /// The backing is another type.
    WrongType(NativeBuffer),
}

impl IntoBackingError {
    /// The buffer, unchanged.
    #[must_use]
    pub fn into_buffer(self) -> NativeBuffer {
        match self {
            Self::Shared(buffer) | Self::WrongType(buffer) => buffer,
        }
    }
}

impl fmt::Display for IntoBackingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Shared(_) => "the native buffer is shared with another clone",
            Self::WrongType(_) => "the native buffer's backing is another type",
        })
    }
}

impl std::error::Error for IntoBackingError {}

/// The release hooks of a buffer whose backing was moved out with
/// [`NativeBuffer::into_backing`]. Dropping it tells the producer the
/// memory is free.
#[must_use = "dropping the lease releases the buffer to its producer at once"]
pub struct BufferLease {
    // A mutex only so the lease is `Sync`; nothing locks it.
    hooks: Mutex<Hooks>,
}

impl Drop for BufferLease {
    fn drop(&mut self) {
        std::mem::take(self.hooks.get_mut().unwrap_or_else(|p| p.into_inner())).run();
    }
}

impl fmt::Debug for BufferLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BufferLease")
            .field("hooks", &lock(&self.hooks).len())
            .finish()
    }
}

/// One plane of a [`DmaBuf`].
#[cfg(unix)]
#[derive(Debug)]
pub struct DmaBufPlane {
    /// The plane's DMA-BUF; closed with the [`DmaBuf`].
    pub fd: OwnedFd,
    /// Byte offset of the plane inside `fd`.
    pub offset: u32,
    /// Row pitch in bytes.
    pub stride: u32,
    /// Size in bytes of the whole DMA-BUF object behind `fd` (what
    /// `lseek(fd, 0, SEEK_END)` reports), which Vulkan and EGL imports
    /// need; `None` when the producer does not know it.
    pub size: Option<u64>,
}

#[cfg(unix)]
impl DmaBufPlane {
    fn is_cloexec(&self) -> bool {
        rustix::io::fcntl_getfd(&self.fd)
            .is_ok_and(|flags| flags.contains(rustix::io::FdFlags::CLOEXEC))
    }
}

/// A Linux DMA-BUF image: what `linux-dmabuf`, `PipeWire`, GBM, VA-API
/// and Vulkan's external memory exchange.
///
/// Synchronisation is implicit in the hand-over: the producer only
/// pushes a frame once its writes are complete (it waited on its
/// acquire fence), and a consumer's reads are complete once it drops
/// the buffer (or its [`BufferLease`]) — a consumer that reads on a GPU
/// waits for that work before dropping.
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
        let (first, second) = (Arc::clone(&order), Arc::clone(&order));
        let buffer = NativeBuffer::new(MemoryKind::Other, Backing(Arc::clone(&order)))
            .with_release(move || lock(&first).push("hook 1"))
            .with_release(move || lock(&second).push("hook 2"));
        let clone = buffer.clone();
        drop(buffer);
        assert!(lock(&order).is_empty(), "a clone still holds it");
        drop(clone);
        assert_eq!(*lock(&order), ["backing", "hook 1", "hook 2"]);
    }

    #[test]
    fn hooks_added_to_a_shared_buffer_run_too() {
        let ran = Arc::new(AtomicUsize::new(0));
        let buffer = NativeBuffer::from_bytes(vec![1u8]);
        let clone = buffer.clone();
        let count = Arc::clone(&ran);
        let buffer = buffer.with_release(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        drop(buffer);
        drop(clone);
        assert_eq!(ran.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn panics_in_releases_are_contained() {
        struct Panics;
        impl Drop for Panics {
            fn drop(&mut self) {
                panic!("backing drop");
            }
        }
        let later = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&later);
        let buffer = NativeBuffer::new(MemoryKind::Other, Panics)
            .with_release(|| panic!("hook 1"))
            .with_release(move || flag.store(true, Ordering::SeqCst));
        drop(buffer);
        assert!(later.load(Ordering::SeqCst), "every hook runs");
    }

    #[test]
    fn into_backing_moves_the_value_and_keeps_the_hooks_in_the_lease() {
        let released = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&released);
        let buffer = NativeBuffer::from_bytes(vec![1u8, 2, 3]).with_release(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });

        let shared = buffer.clone();
        let err = buffer
            .into_backing::<Vec<u8>>()
            .expect_err("shared buffers keep their backing");
        assert!(matches!(err, IntoBackingError::Shared(_)));
        drop(shared);

        let err = err
            .into_buffer()
            .into_backing::<Probe>()
            .expect_err("wrong type gives the buffer back");
        assert!(matches!(err, IntoBackingError::WrongType(_)));
        let buffer = err.into_buffer();
        assert_eq!(buffer.as_bytes(), Some(&[1u8, 2, 3][..]));

        let (cpu, lease) = buffer
            .into_backing::<Vec<u8>>()
            .expect("unique, right type");
        assert_eq!(cpu, [1, 2, 3]);
        assert_eq!(released.load(Ordering::SeqCst), 0);
        drop(lease);
        assert_eq!(released.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn leases_are_send_and_sync() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<BufferLease>();
        send_sync::<NativeBuffer>();
    }

    #[test]
    fn downcast_kind_identity_and_platform_pointer() {
        // A token address stands in for a platform object.
        struct Token(usize);
        impl PlatformBuffer for Token {
            fn kind(&self) -> MemoryKind {
                MemoryKind::IoSurface
            }
            fn as_ptr(&self) -> NonNull<c_void> {
                NonNull::new(self.0 as *mut c_void).unwrap_or(NonNull::dangling())
            }
        }
        let buffer = NativeBuffer::from_platform(Token(0x1000))
            .with_identity(BufferIdentity::new(7, 2))
            .with_identity(BufferIdentity::new(8, 3));
        assert_eq!(buffer.kind(), MemoryKind::IoSurface);
        assert_eq!(
            buffer.platform_ptr().map(|p| p.as_ptr() as usize),
            Some(0x1000)
        );
        assert_eq!(buffer.identity(), Some(BufferIdentity::new(7, 2)));
        assert!(buffer.as_bytes().is_none());

        let drops = Arc::new(AtomicUsize::new(0));
        let buffer = NativeBuffer::new(MemoryKind::HardwareBuffer, Probe(Arc::clone(&drops)));
        assert!(buffer.downcast_ref::<Probe>().is_some());
        assert!(buffer.platform_ptr().is_none());
        drop(buffer);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_ne!(
            BufferIdentity::next_generation(),
            BufferIdentity::next_generation()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[cfg_attr(miri, ignore = "opens /dev/null and reads /proc")]
    fn dmabuf_fds_close_when_the_last_arc_drops() {
        use std::os::fd::AsRawFd;

        let file = std::fs::File::open("/dev/null").expect("open /dev/null");
        let fd: OwnedFd = file.into();
        let raw = fd.as_raw_fd();
        let pooled = Arc::new(DmaBuf {
            width: 4,
            height: 4,
            fourcc: u32::from_le_bytes(*b"XR24"),
            modifier: 0,
            planes: vec![DmaBufPlane {
                fd,
                offset: 0,
                stride: 16,
                size: Some(64),
            }],
        });
        let buffer = NativeBuffer::from_dmabuf(Arc::clone(&pooled));
        assert_eq!(buffer.dmabuf().map(|d| d.planes.len()), Some(1));
        drop(buffer);
        let link = || std::fs::read_link(format!("/proc/self/fd/{raw}"));
        assert_eq!(
            link().ok().as_deref(),
            Some(std::path::Path::new("/dev/null")),
            "the pool still holds it"
        );
        drop(pooled);
        // The descriptor number is free again (or reused by someone
        // else, which `fcntl` cannot tell apart); `/proc` says which.
        assert!(link().map_or(true, |path| path != std::path::Path::new("/dev/null")));
    }
}
