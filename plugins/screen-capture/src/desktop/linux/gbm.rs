//! GBM DMA-BUF allocation for the direct Wayland backends, and the slot
//! pool their capture threads reuse.
//!
//! Buffers are allocated on the device the compositor renders with
//! (`linux-dmabuf` feedback main device, or the ext-image-copy session's
//! `dmabuf_device`), so the compositor can import them. The modifier list
//! is the caller's tier ∩ what the compositor imports
//! (`crate::select::negotiate`); the implicit modifier is never passed
//! unless listed, and LINEAR is never assumed (NVIDIA's GBM refuses
//! LINEAR render targets).

#![cfg_attr(
    not(any(feature = "linux-wlr-screencopy", feature = "linux-ext-image-copy")),
    allow(
        dead_code,
        reason = "the portal alone uses only the dmabuf feedback and the driver check"
    )
)]

use std::fs::File;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;

use istmo_core::{DmaBuf, DmaBufPlane};

use crate::pool::{Exhausted, PoolAccounting};

/// Why a buffer could not be allocated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AllocError {
    /// No render node could be opened (or none matched the device).
    NoDevice(String),
    /// GBM refused every listed modifier.
    Refused { fourcc: u32, reason: String },
    /// The buffer could not be exported as DMA-BUF.
    Export(String),
}

impl std::fmt::Display for AllocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDevice(why) => write!(f, "no GBM device: {why}"),
            Self::Refused { fourcc, reason } => {
                write!(f, "GBM refused fourcc 0x{fourcc:08x}: {reason}")
            }
            Self::Export(why) => write!(f, "DMA-BUF export failed: {why}"),
        }
    }
}

impl std::error::Error for AllocError {}

/// `/dev/dri/renderD128..` in numerical order.
fn render_nodes() -> Vec<String> {
    (128..=192)
        .map(|n| format!("/dev/dri/renderD{n}"))
        .filter(|p| Path::new(p).exists())
        .collect()
}

/// The render node of the DRM device `dev_t` names (a `card*` primary
/// node announced as main device).
fn sibling_render_node(dev_t: u64) -> Option<String> {
    let major = rustix::fs::major(dev_t);
    let minor = rustix::fs::minor(dev_t);
    let dir = format!("/sys/dev/char/{major}:{minor}/device/drm");
    std::fs::read_dir(dir).ok()?.find_map(|entry| {
        let name = entry.ok()?.file_name().into_string().ok()?;
        name.starts_with("renderD")
            .then(|| format!("/dev/dri/{name}"))
    })
}

/// Whether the DRM device `dev_t` is driven by NVIDIA's proprietary
/// driver (which attaches no implicit fences).
pub(crate) fn is_nvidia(dev_t: u64) -> bool {
    let link = format!(
        "/sys/dev/char/{}:{}/device/driver",
        rustix::fs::major(dev_t),
        rustix::fs::minor(dev_t)
    );
    std::fs::read_link(link)
        .ok()
        .and_then(|target| target.file_name().map(|n| n == "nvidia"))
        .unwrap_or(false)
}

/// A render node as a DRM device (PRIME export needs the trait).
pub(crate) struct Node(File);

impl AsFd for Node {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Node {}
impl drm::control::Device for Node {}

/// Close-on-exec after the fact, for the fallback export path only.
fn cloexec(fd: &OwnedFd) -> Result<(), AllocError> {
    let flags = rustix::io::fcntl_getfd(fd).map_err(|e| AllocError::Export(e.to_string()))?;
    rustix::io::fcntl_setfd(fd, flags | rustix::io::FdFlags::CLOEXEC)
        .map_err(|e| AllocError::Export(e.to_string()))
}

/// A GBM device on one render node.
pub(crate) struct Allocator {
    device: gbm::Device<Node>,
    /// `st_rdev` of the render node: the producer GPU.
    pub(crate) dev_t: u64,
}

impl std::fmt::Debug for Allocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Allocator")
            .field("dev_t", &format_args!("0x{:x}", self.dev_t))
            .finish_non_exhaustive()
    }
}

impl Allocator {
    /// The render node of `device`; without one, the first node that
    /// opens (single-GPU hosts, compositors without feedback).
    ///
    /// # Errors
    /// [`AllocError::NoDevice`] when no node matches or opens.
    pub(crate) fn open(device: Option<u64>) -> Result<Self, AllocError> {
        let open = |node: &str| -> Option<Self> {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(node)
                .ok()?;
            let dev_t = file.metadata().ok()?.rdev();
            let device = gbm::Device::new(Node(file)).ok()?;
            Some(Self { device, dev_t })
        };
        let nodes = render_nodes();
        if let Some(wanted) = device {
            let matching = nodes
                .iter()
                .find(|node| std::fs::metadata(node).is_ok_and(|m| m.rdev() == wanted))
                .cloned()
                .or_else(|| sibling_render_node(wanted));
            return matching.and_then(|node| open(&node)).ok_or_else(|| {
                AllocError::NoDevice(format!("no render node of device 0x{wanted:x} opens"))
            });
        }
        nodes
            .iter()
            .find_map(|node| open(node))
            .ok_or_else(|| AllocError::NoDevice("no /dev/dri/renderD* opens".to_owned()))
    }

    /// A `width`×`height` render target in `fourcc`, with one of
    /// `modifiers` (in preference order; GBM picks among them).
    ///
    /// # Errors
    /// GBM refused, or the export failed.
    pub(crate) fn alloc(
        &self,
        width: u32,
        height: u32,
        fourcc: u32,
        modifiers: &[u64],
    ) -> Result<Bo, AllocError> {
        let refused = |reason: String| AllocError::Refused { fourcc, reason };
        let format = drm_fourcc::DrmFourcc::try_from(fourcc).map_err(|e| refused(e.to_string()))?;
        if modifiers.is_empty() {
            return Err(refused("no modifier both sides accept".to_owned()));
        }
        // `RENDERING`: the compositor binds the buffer as a render target.
        let bo = self
            .device
            .create_buffer_object_with_modifiers2::<()>(
                width,
                height,
                format,
                modifiers.iter().map(|m| gbm::Modifier::from(*m)),
                gbm::BufferObjectFlags::RENDERING,
            )
            .map_err(|e| refused(e.to_string()))?;
        let modifier = u64::from(bo.modifier());
        let handles = drm::buffer::PlanarBuffer::handles(&bo);
        let planes = (0..bo.plane_count().max(1))
            .map(|plane| {
                let index = i32::try_from(plane).unwrap_or(0);
                let handle = usize::try_from(plane)
                    .ok()
                    .and_then(|i| handles.get(i).copied().flatten());
                let fd = self.export(&bo, index, handle)?;
                let size = rustix::fs::seek(&fd, rustix::fs::SeekFrom::End(0)).ok();
                Ok(DmaBufPlane {
                    fd,
                    offset: bo.offset(index),
                    stride: bo.stride_for_plane(index),
                    size,
                })
            })
            .collect::<Result<Vec<_>, AllocError>>()?;
        Ok(Bo {
            dmabuf: Arc::new(DmaBuf {
                width,
                height,
                fourcc,
                modifier,
                planes,
            }),
            _bo: bo,
        })
    }

    /// One plane of `bo` as a DMA-BUF fd, close-on-exec from creation:
    /// PRIME export of the GEM handle with `DRM_CLOEXEC`. A driver whose
    /// GBM handles cannot be exported that way falls back to GBM's own
    /// export (which Mesa and NVIDIA open with `O_CLOEXEC` too) and the
    /// flag is set again.
    fn export(
        &self,
        bo: &gbm::BufferObject<()>,
        plane: i32,
        handle: Option<drm::buffer::Handle>,
    ) -> Result<OwnedFd, AllocError> {
        if let Some(handle) = handle {
            match drm::control::Device::buffer_to_prime_fd(
                &*self.device,
                handle,
                drm::CLOEXEC | drm::RDWR,
            ) {
                Ok(fd) => return Ok(fd),
                Err(err) => {
                    tracing::debug!(%err, "screen-capture: PRIME export refused; using GBM's");
                }
            }
        }
        let fd = bo
            .fd_for_plane(plane)
            .map_err(|e| AllocError::Export(e.to_string()))?;
        cloexec(&fd)?;
        Ok(fd)
    }
}

/// One allocated buffer: the GBM object (kept so the memory stays owned
/// here) and its DMA-BUF, shared with every frame cut from it (no `dup`
/// per frame, stable fd numbers).
pub(crate) struct Bo {
    pub(crate) dmabuf: Arc<DmaBuf>,
    _bo: gbm::BufferObject<()>,
}

impl std::fmt::Debug for Bo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bo")
            .field("dmabuf", &self.dmabuf)
            .finish_non_exhaustive()
    }
}

/// What the pool offers for the next frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pick {
    /// Reuse this free slot.
    Free(u32),
    /// Allocate a new buffer.
    Grow,
    /// The consumer holds every buffer it may.
    Exhausted(Exhausted),
}

/// The buffers of one capture, keyed by what they were allocated for.
/// A key change retires every buffer: free ones are dropped, held ones
/// stay alive with their frames (their `DmaBuf` is shared) and their
/// late releases are inert (new pool generation).
#[derive(Debug)]
pub(crate) struct Slots<K, T> {
    accounting: PoolAccounting,
    key: Option<K>,
    entries: Vec<Option<T>>,
    depth: usize,
}

impl<K: PartialEq, T> Slots<K, T> {
    pub(crate) fn new(depth: usize) -> Self {
        Self {
            accounting: PoolAccounting::new(depth),
            key: None,
            entries: Vec::new(),
            depth,
        }
    }

    pub(crate) const fn accounting(&self) -> &PoolAccounting {
        &self.accounting
    }

    /// Switch to `key`; `true` when that retired buffers (a new
    /// generation began).
    pub(crate) fn reconfigure(&mut self, key: K) -> bool {
        if self.key.as_ref() == Some(&key) {
            return false;
        }
        let first = self.key.is_none();
        self.key = Some(key);
        self.entries.clear();
        if !first {
            let _generation = self.accounting.reallocate(self.depth);
        }
        !first
    }

    pub(crate) fn pick(&self) -> Pick {
        if let Err(exhausted) = self.accounting.check_room() {
            return Pick::Exhausted(exhausted);
        }
        let free = self.entries.iter().enumerate().find_map(|(i, e)| {
            let slot = u32::try_from(i).ok()?;
            (e.is_some() && !self.accounting.is_held(slot)).then_some(slot)
        });
        // Without a free buffer, grow: the accounting has room, so a
        // held buffer was discarded or the pool is still filling.
        free.map_or(Pick::Grow, Pick::Free)
    }

    /// Store a new buffer, reusing a tombstone; its slot index.
    pub(crate) fn insert(&mut self, value: T) -> u32 {
        let index = if let Some(i) = self.entries.iter().position(Option::is_none) {
            if let Some(entry) = self.entries.get_mut(i) {
                *entry = Some(value);
            }
            i
        } else {
            self.entries.push(Some(value));
            self.entries.len() - 1
        };
        u32::try_from(index).unwrap_or(u32::MAX)
    }

    pub(crate) fn get(&self, slot: u32) -> Option<&T> {
        self.entries.get(usize::try_from(slot).ok()?)?.as_ref()
    }

    /// Forget a buffer the compositor refused (a held one stays until the
    /// next key change).
    pub(crate) fn discard(&mut self, slot: u32) -> Option<T> {
        if self.accounting.is_held(slot) {
            return None;
        }
        self.entries.get_mut(usize::try_from(slot).ok()?)?.take()
    }

    /// Buffers allocated.
    #[cfg(test)]
    pub(crate) fn alive(&self) -> usize {
        self.entries.iter().filter(|e| e.is_some()).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> Slots<u32, &'static str> {
        let mut slots = Slots::new(3);
        assert!(!slots.reconfigure(1), "the first key is no reallocation");
        slots
    }

    #[test]
    fn unleased_buffer_is_reused() {
        let mut slots = pool();
        assert_eq!(slots.pick(), Pick::Grow);
        let a = slots.insert("a");
        assert_eq!(slots.pick(), Pick::Free(a));
    }

    #[test]
    fn leased_buffer_is_skipped_until_released() {
        let mut slots = pool();
        let a = slots.insert("a");
        let lease = slots.accounting().hold(a).expect("room");
        assert_eq!(slots.pick(), Pick::Grow);
        let b = slots.insert("b");
        assert_eq!(slots.pick(), Pick::Free(b));
        drop(lease);
        assert_eq!(slots.pick(), Pick::Free(a));
    }

    #[test]
    fn exhausted_at_cap_when_everything_is_leased() {
        let mut slots = pool();
        let a = slots.insert("a");
        let b = slots.insert("b");
        let _la = slots.accounting().hold(a).expect("room");
        let _lb = slots.accounting().hold(b).expect("room");
        assert!(matches!(slots.pick(), Pick::Exhausted(e) if e.new_episode));
    }

    #[test]
    fn key_change_drops_free_and_retires_leased_buffers() {
        let mut slots = pool();
        let a = slots.insert("a");
        let lease = slots.accounting().hold(a).expect("room");
        let generation = lease.identity().pool_generation;
        assert!(slots.reconfigure(2));
        assert_eq!(slots.alive(), 0);
        assert_ne!(slots.accounting().generation(), generation);
        drop(lease);
        assert_eq!(
            slots.accounting().in_flight(),
            0,
            "the late release is inert"
        );
    }

    #[test]
    fn discard_removes_only_unleased_buffers() {
        let mut slots = pool();
        let a = slots.insert("a");
        let b = slots.insert("b");
        let _lease = slots.accounting().hold(a).expect("room");
        assert!(slots.discard(a).is_none());
        assert_eq!(slots.discard(b), Some("b"));
        assert_eq!(slots.alive(), 1);
        assert_eq!(slots.insert("c"), b, "the tombstone is reused");
    }

    #[test]
    fn stale_index_is_rejected() {
        let slots = pool();
        assert!(slots.get(7).is_none());
    }
}
