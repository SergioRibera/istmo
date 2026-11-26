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
//!
//! A short file would make the first read past its end `SIGBUS` the
//! process, so every mapping checks the file's size first, and the
//! plugin's own memfds ([`SealedMemfd`]) are sealed against resizing: a
//! peer holding the descriptor can neither shrink nor grow them.

#![allow(
    unsafe_code,
    reason = "mmap / munmap of shared-memory frames: the FFI this module wraps"
)]
#![allow(
    dead_code,
    reason = "each shared-memory backend uses a different part; which depends on the enabled features"
)]

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::ptr::NonNull;
use std::sync::Arc;

/// One `MAP_SHARED` mapping, unmapped on drop. Read-only, or writable for
/// the plugin's own segments (an X11 cursor is blended into the frame).
#[derive(Debug)]
pub(crate) struct Mapping {
    writable: bool,
    /// Start of the page-aligned mapping.
    base: NonNull<u8>,
    /// Start of the caller's range (`base + skip`).
    start: NonNull<u8>,
    /// Bytes mapped from `base`.
    mapped: usize,
    /// Bytes of the caller's range.
    len: usize,
}

// SAFETY: the mapping is plain memory owned by this value with no thread
// affinity; shared access is `&[u8]` only, writes need `&mut self`.
unsafe impl Send for Mapping {}
// SAFETY: as above: through `&self` only reads happen, from any thread.
unsafe impl Sync for Mapping {}

/// Why a mapping failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MapError {
    /// Zero bytes, or a range that overflows.
    BadRange { offset: u64, len: usize },
    /// The file ends before the range does (`size < end`): reading the
    /// tail would `SIGBUS`.
    ShortFile { size: u64, end: u64 },
    /// The OS refused (`errno`).
    Os(i32),
}

impl std::fmt::Display for MapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadRange { offset, len } => write!(f, "cannot map {len} bytes at {offset}"),
            Self::ShortFile { size, end } => {
                write!(f, "the file holds {size} bytes, the mapping needs {end}")
            }
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
    /// An empty or overflowing range, a file shorter than `offset + len`
    /// (checked with `fstat`), or the OS refusing the mapping.
    pub(crate) fn new(fd: impl AsFd, offset: u64, len: usize) -> Result<Self, MapError> {
        Self::map(fd, offset, len, false)
    }

    /// [`new`](Self::new), writable through [`bytes_mut`](Self::bytes_mut).
    /// For memory the plugin created (a memfd it shares with a producer).
    ///
    /// # Errors
    /// As [`new`](Self::new).
    pub(crate) fn new_writable(fd: impl AsFd, len: usize) -> Result<Self, MapError> {
        Self::map(fd, 0, len, true)
    }

    fn map(fd: impl AsFd, offset: u64, len: usize, writable: bool) -> Result<Self, MapError> {
        let bad = || MapError::BadRange { offset, len };
        if len == 0 {
            return Err(bad());
        }
        let page = rustix::param::page_size() as u64;
        let aligned = offset - offset % page;
        let skip = usize::try_from(offset - aligned).map_err(|_| bad())?;
        let mapped = skip.checked_add(len).ok_or_else(bad)?;
        let end = u64::try_from(len)
            .ok()
            .and_then(|len| offset.checked_add(len))
            .ok_or_else(bad)?;
        let size = rustix::fs::fstat(&fd)
            .map_err(|err| MapError::Os(err.raw_os_error()))?
            .st_size;
        let size = u64::try_from(size).unwrap_or(0);
        if size < end {
            return Err(MapError::ShortFile { size, end });
        }
        // SAFETY: a fresh mapping chosen by the kernel (null hint), not
        // aliasing any Rust allocation; `fd` is a live descriptor for the
        // duration of the call. The memory is only read through `&[u8]`
        // (see the module docs for why it does not change while read).
        let base = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                mapped,
                if writable {
                    rustix::mm::ProtFlags::READ | rustix::mm::ProtFlags::WRITE
                } else {
                    rustix::mm::ProtFlags::READ
                },
                rustix::mm::MapFlags::SHARED,
                fd,
                aligned,
            )
        }
        .map_err(|err| MapError::Os(err.raw_os_error()))?;
        let base = NonNull::new(base.cast::<u8>()).ok_or_else(bad)?;
        // SAFETY: `mapped = skip + len` with `len > 0`, so `skip < mapped`
        // and the offset stays inside the mapping just created.
        let start = unsafe { base.add(skip) };
        Ok(Self {
            writable,
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

    /// The mapped range, writable: `None` for a read-only mapping. `&mut
    /// self` proves no frame reads it (frames hold `Arc<Mapping>`, so a
    /// caller reaches this only through `Arc::get_mut`).
    pub(crate) const fn bytes_mut(&mut self) -> Option<&mut [u8]> {
        if !self.writable {
            return None;
        }
        // SAFETY: a live writable mapping of `len` bytes from `start`,
        // owned by `self` and borrowed exclusively for the result's life.
        Some(unsafe { std::slice::from_raw_parts_mut(self.start.as_ptr(), self.len) })
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

/// A memfd the plugin creates and shares with a producer (a compositor's
/// `wl_shm` pool, an X server's MIT-SHM segment), sized once and sealed
/// with `F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_SEAL`: the peer cannot
/// truncate it under a live mapping.
#[derive(Debug)]
pub(crate) struct SealedMemfd(OwnedFd);

impl SealedMemfd {
    /// A fresh close-on-exec memfd of `len` zero bytes, sealed.
    ///
    /// # Errors
    /// `memfd_create`, `ftruncate` or adding the seals failed; the step
    /// and its `errno`.
    pub(crate) fn new(name: &str, len: u64) -> Result<Self, (&'static str, rustix::io::Errno)> {
        use rustix::fs::{MemfdFlags, SealFlags};
        let fd = rustix::fs::memfd_create(name, MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)
            .map_err(|e| ("memfd_create", e))?;
        rustix::fs::ftruncate(&fd, len).map_err(|e| ("ftruncate", e))?;
        rustix::fs::fcntl_add_seals(&fd, SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL)
            .map_err(|e| ("seal memfd", e))?;
        Ok(Self(fd))
    }
}

impl AsFd for SealedMemfd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl From<SealedMemfd> for OwnedFd {
    fn from(memfd: SealedMemfd) -> Self {
        memfd.0
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
    /// `range` of `mapping`, or `None` when it does not lie inside.
    pub(crate) fn new(mapping: Arc<Mapping>, range: std::ops::Range<usize>) -> Option<Self> {
        mapping.bytes().get(range.clone())?;
        Some(Self { mapping, range })
    }

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

    #[test]
    fn a_range_past_the_end_of_the_file_is_refused() {
        let page = rustix::param::page_size();
        let fd = memfd(&vec![7; page + 16]);
        let short = Err(MapError::ShortFile {
            size: (page + 16) as u64,
            end: (page + 17) as u64,
        });
        assert_eq!(Mapping::new(&fd, 0, page + 17).map(|_| ()), short);
        assert_eq!(Mapping::new(&fd, 8, page + 9).map(|_| ()), short);
        assert!(Mapping::new(&fd, 8, page + 8).is_ok(), "exactly to the end");
        assert!(matches!(
            Mapping::new(&fd, u64::MAX, 2),
            Err(MapError::BadRange { .. })
        ));
    }

    #[test]
    fn a_sealed_memfd_cannot_be_resized() {
        use rustix::fs::SealFlags;
        let memfd = SealedMemfd::new("sealed-test", 4096).expect("sealed memfd");
        assert_eq!(rustix::fs::fstat(&memfd).expect("fstat").st_size, 4096);
        assert!(rustix::fs::ftruncate(&memfd, 16).is_err(), "cannot shrink");
        assert!(rustix::fs::ftruncate(&memfd, 8192).is_err(), "cannot grow");
        let seals = rustix::fs::fcntl_get_seals(&memfd).expect("seals");
        assert!(seals.contains(SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL));
        let mut mapping = Mapping::new_writable(&memfd, 4096).expect("map");
        mapping.bytes_mut().expect("writable")[0] = 9;
        assert_eq!(mapping.bytes()[0], 9);
    }
}
