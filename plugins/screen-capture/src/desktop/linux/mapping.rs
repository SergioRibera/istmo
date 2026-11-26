//! Read-only mappings of shared-memory frames (`wl_shm` pools, `XShm`
//! segments, PipeWire `MemFd` buffers), handed to consumers as the
//! frame's CPU bytes without a copy.
//!
//! A mapping only ever exposes `&[u8]`. The producer (compositor, X
//! server, PipeWire producer) writes into the same memory, but only while
//! the plugin's pool accounting says the slot is free: a slot is handed
//! back to the producer once every frame cut from it dropped, and a
//! frame keeps its mapping alive. The bytes a consumer reads therefore
//! never change under it.

#![allow(
    unsafe_code,
    reason = "mmap / munmap of shared-memory frames: the FFI this module wraps"
)]

use std::os::fd::AsFd;
use std::ptr::NonNull;
use std::sync::Arc;

/// One read-only `MAP_SHARED` mapping, unmapped on drop.
#[derive(Debug)]
pub(crate) struct Mapping {
    /// Start of the page-aligned mapping.
    base: NonNull<u8>,
    /// Start of the caller's range (`base + skip`).
    start: NonNull<u8>,
    /// Bytes mapped from `base`.
    mapped: usize,
    /// Bytes of the caller's range.
    len: usize,
}

// SAFETY: the mapping is plain read-only memory owned by this value; it
// carries no thread affinity, and every access goes through `&[u8]`.
unsafe impl Send for Mapping {}
// SAFETY: as above: only shared reads, which are fine from any thread.
unsafe impl Sync for Mapping {}

/// Why a mapping failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MapError {
    /// Zero bytes, or a range that overflows.
    BadRange { offset: u64, len: usize },
    /// The OS refused (`errno`).
    Os(i32),
}

impl std::fmt::Display for MapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadRange { offset, len } => write!(f, "cannot map {len} bytes at {offset}"),
            Self::Os(errno) => write!(f, "mmap failed (errno {errno})"),
        }
    }
}

impl std::error::Error for MapError {}

impl Mapping {
    /// Map `len` bytes of `fd` from `offset`, read-only and shared.
    ///
    /// The fd may be closed afterwards; the mapping keeps the memory.
    ///
    /// # Errors
    /// An empty or overflowing range, or the OS refusing the mapping
    /// (a short file is caught by the kernel only on access: callers map
    /// sizes the producer guaranteed, `ftruncate`d or announced).
    pub(crate) fn new(fd: impl AsFd, offset: u64, len: usize) -> Result<Self, MapError> {
        let bad = || MapError::BadRange { offset, len };
        if len == 0 {
            return Err(bad());
        }
        let page = rustix::param::page_size() as u64;
        let aligned = offset - offset % page;
        let skip = usize::try_from(offset - aligned).map_err(|_| bad())?;
        let mapped = skip.checked_add(len).ok_or_else(bad)?;
        // SAFETY: a fresh mapping chosen by the kernel (null hint), not
        // aliasing any Rust allocation; `fd` is a live descriptor for the
        // duration of the call. The memory is only read through `&[u8]`
        // (see the module docs for why it does not change while read).
        let base = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                mapped,
                rustix::mm::ProtFlags::READ,
                rustix::mm::MapFlags::SHARED,
                fd,
                aligned,
            )
        }
        .map_err(|err| MapError::Os(err.raw_os_error()))?;
        let base = NonNull::new(base.cast::<u8>()).ok_or_else(bad)?;
        // SAFETY: `skip < page <= mapped`, so the offset stays inside the
        // mapping just created.
        let start = unsafe { base.add(skip) };
        Ok(Self {
            base,
            start,
            mapped,
            len,
        })
    }

    /// The mapped range.
    pub(crate) const fn bytes(&self) -> &[u8] {
        // SAFETY: `start..start + len` ends exactly at the end of the live
        // read-only mapping owned by `self`; the borrow cannot outlive
        // `self`, which unmaps on drop.
        unsafe { std::slice::from_raw_parts(self.start.as_ptr(), self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: `base` / `mapped` are exactly what `mmap` returned, and
        // no borrow of the memory outlives `self`.
        let unmapped = unsafe { rustix::mm::munmap(self.base.as_ptr().cast(), self.mapped) };
        if let Err(err) = unmapped {
            tracing::warn!(%err, "screen-capture: munmap failed");
        }
    }
}

/// A frame's bytes: `range` of a shared mapping. What a consumer gets
/// from `NativeBuffer::as_bytes`; clones of the frame share the mapping.
#[derive(Debug, Clone)]
pub(crate) struct MappedFrame {
    mapping: Arc<Mapping>,
    range: std::ops::Range<usize>,
}

impl MappedFrame {
    /// The whole mapping.
    pub(crate) fn whole(mapping: Arc<Mapping>) -> Self {
        let range = 0..mapping.bytes().len();
        Self { mapping, range }
    }
}

impl AsRef<[u8]> for MappedFrame {
    fn as_ref(&self) -> &[u8] {
        self.mapping.bytes().get(self.range.clone()).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memfd(bytes: &[u8]) -> std::os::fd::OwnedFd {
        let fd = rustix::fs::memfd_create("mapping-test", rustix::fs::MemfdFlags::CLOEXEC)
            .expect("memfd");
        rustix::io::write(&fd, bytes).expect("write");
        fd
    }

    #[test]
    fn a_mapping_shows_the_file_from_any_offset() {
        let page = rustix::param::page_size();
        let bytes: Vec<u8> = (0..page * 2 + 10)
            .map(|i| u8::try_from(i % 251).expect("below 251"))
            .collect();
        let fd = memfd(&bytes);
        let whole = Mapping::new(&fd, 0, bytes.len()).expect("map");
        assert_eq!(whole.bytes(), &bytes[..]);
        // An unaligned offset past the first page.
        let tail = Mapping::new(&fd, (page + 3) as u64, 7).expect("map tail");
        assert_eq!(tail.bytes(), &bytes[page + 3..page + 10]);
        drop(fd);
        assert_eq!(whole.bytes()[5], bytes[5], "outlives the fd");
    }

    #[test]
    fn frames_show_the_whole_mapping() {
        let fd = memfd(&[1, 2, 3, 4]);
        let mapping = Arc::new(Mapping::new(&fd, 0, 4).expect("map"));
        assert_eq!(MappedFrame::whole(mapping).as_ref(), &[1, 2, 3, 4]);
        assert!(matches!(
            Mapping::new(&fd, 0, 0),
            Err(MapError::BadRange { .. })
        ));
    }
}
