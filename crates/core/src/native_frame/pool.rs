//! Recycled CPU buffers for producers that copy frames in.
//!
//! A fresh multi-megabyte `Vec` per frame costs a `mmap` and a page
//! fault per 4 KiB page; reusing the buffers the consumer dropped turns
//! the copy into a plain `memcpy` over memory that is already mapped.

use std::sync::{Arc, Mutex, Weak};

use super::buffer::NativeBuffer;
use crate::sync::lock;

/// Buffers a [`BytePool`] keeps by default: a queue of a few frames plus
/// the one being filled and the one being read.
pub const DEFAULT_POOLED_BUFFERS: usize = 6;

/// Bytes a [`BytePool`] keeps by default (beyond that, returned buffers
/// are freed).
pub const DEFAULT_POOLED_BYTES: usize = 96 << 20;

/// A pooled buffer serves a frame of at most this many times less than
/// its capacity: a small frame queued for long must not hold a buffer
/// sized for a large one.
const MAX_SLACK: usize = 4;

/// Buffers up to this capacity fit any smaller frame.
const SMALL_BUFFER: usize = 64 << 10;

/// Recycles the byte buffers of copied frames.
///
/// A frame built with [`copy_of`](Self::copy_of) or [`buffer`](Self::buffer) returns its
/// memory here when the consumer drops it, and the next frame of a
/// fitting size reuses it.
///
/// Cloning shares the pool. Frames outliving the pool just free their
/// memory.
#[derive(Debug, Clone)]
pub struct BytePool {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    free: Mutex<Free>,
    max_buffers: usize,
    max_bytes: usize,
}

#[derive(Debug, Default)]
struct Free {
    buffers: Vec<Vec<u8>>,
    /// Capacity of `buffers`, summed.
    bytes: usize,
}

impl Default for BytePool {
    fn default() -> Self {
        Self::new(DEFAULT_POOLED_BUFFERS, DEFAULT_POOLED_BYTES)
    }
}

impl BytePool {
    /// A pool keeping at most `max_buffers` buffers and `max_bytes` bytes.
    #[must_use]
    pub fn new(max_buffers: usize, max_bytes: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                free: Mutex::new(Free::default()),
                max_buffers,
                max_bytes,
            }),
        }
    }

    /// An empty buffer with room for at least `len` bytes: the smallest
    /// pooled one that fits without being much larger (at most
    /// 4 × `len`, or 64 KiB), or a new one.
    #[must_use]
    pub fn take(&self, len: usize) -> Vec<u8> {
        let mut free = lock(&self.inner.free);
        let best = free
            .buffers
            .iter()
            .enumerate()
            .filter(|(_, buffer)| fits(buffer.capacity(), len))
            .min_by_key(|(_, buffer)| buffer.capacity())
            .map(|(index, _)| index);
        let reused = best.map(|index| free.buffers.swap_remove(index));
        if let Some(buffer) = &reused {
            free.bytes -= buffer.capacity();
        }
        drop(free);
        reused.map_or_else(
            || Vec::with_capacity(len),
            |mut buffer| {
                buffer.clear();
                buffer
            },
        )
    }

    /// A frame buffer over `bytes` (usually from [`take`](Self::take))
    /// that comes back to this pool once dropped.
    #[must_use]
    pub fn buffer(&self, bytes: Vec<u8>) -> NativeBuffer {
        NativeBuffer::from_bytes(Pooled {
            bytes,
            pool: Arc::downgrade(&self.inner),
        })
    }

    /// A pooled frame buffer holding a copy of `data`.
    #[must_use]
    pub fn copy_of(&self, data: &[u8]) -> NativeBuffer {
        let mut bytes = self.take(data.len());
        bytes.extend_from_slice(data);
        self.buffer(bytes)
    }

    /// Buffers waiting for reuse (tests, diagnostics).
    #[must_use]
    pub fn pooled(&self) -> usize {
        lock(&self.inner.free).buffers.len()
    }
}

/// Whether a buffer of `capacity` may serve a frame of `len` bytes.
const fn fits(capacity: usize, len: usize) -> bool {
    let most = len.saturating_mul(MAX_SLACK);
    capacity >= len && (capacity <= most || capacity <= SMALL_BUFFER)
}

impl Inner {
    fn give_back(&self, bytes: Vec<u8>) {
        let capacity = bytes.capacity();
        if capacity == 0 {
            return;
        }
        let mut free = lock(&self.free);
        if free.buffers.len() < self.max_buffers && free.bytes + capacity <= self.max_bytes {
            free.bytes += capacity;
            free.buffers.push(bytes);
        }
    }
}

/// The backing of a pooled frame.
struct Pooled {
    bytes: Vec<u8>,
    pool: Weak<Inner>,
}

impl AsRef<[u8]> for Pooled {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for Pooled {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade() {
            pool.give_back(std::mem::take(&mut self.bytes));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_frames_lend_their_memory_to_the_next() {
        let pool = BytePool::default();
        let first = pool.copy_of(&[1u8; 1024]);
        let address = first.as_bytes().map(<[u8]>::as_ptr);
        assert_eq!(pool.pooled(), 0);
        drop(first);
        assert_eq!(pool.pooled(), 1);
        let second = pool.copy_of(&[2u8; 512]);
        assert_eq!(second.as_bytes().map(<[u8]>::as_ptr), address, "reused");
        assert_eq!(second.as_bytes(), Some(&[2u8; 512][..]));
        assert_eq!(pool.pooled(), 0);
    }

    #[test]
    fn the_smallest_fitting_buffer_is_reused() {
        let pool = BytePool::default();
        drop(pool.copy_of(&[0u8; 4096]));
        drop(pool.copy_of(&[0u8; 64]));
        assert_eq!(pool.pooled(), 1, "the 4 KiB buffer was reused for 64 bytes");
        let big = pool.take(100_000);
        assert!(big.capacity() >= 100_000, "nothing fits: a new buffer");
        assert_eq!(pool.pooled(), 1);
    }

    #[test]
    fn a_small_frame_never_takes_a_much_larger_buffer() {
        let pool = BytePool::default();
        drop(pool.copy_of(&vec![0u8; 1 << 20]));
        let small = pool.take(1000);
        assert!(
            small.capacity() < 1 << 20,
            "a fresh buffer, not the 1 MiB one"
        );
        assert_eq!(pool.pooled(), 1);
        let quarter = pool.take(256 << 10);
        assert!(quarter.capacity() >= 1 << 20, "within 4x: reused");
        assert_eq!(pool.pooled(), 0);
    }

    #[test]
    fn limits_bound_what_the_pool_keeps() {
        let pool = BytePool::new(2, 10_000);
        let frames: Vec<_> = (0..4).map(|_| pool.copy_of(&[0u8; 1000])).collect();
        drop(frames);
        assert_eq!(pool.pooled(), 2, "buffer count cap");
        let pool = BytePool::new(8, 1500);
        drop(pool.copy_of(&[0u8; 1000]));
        drop(pool.copy_of(&[0u8; 1000]));
        drop((pool.copy_of(&[0u8; 1000]), pool.copy_of(&[0u8; 1000])));
        assert_eq!(pool.pooled(), 1, "byte cap");
    }

    #[test]
    fn frames_outliving_the_pool_just_free() {
        let pool = BytePool::default();
        let frame = pool.copy_of(&[3u8; 16]);
        drop(pool);
        assert_eq!(frame.as_bytes(), Some(&[3u8; 16][..]));
        drop(frame);
    }
}
