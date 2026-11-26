//! Raw access to PipeWire buffers: dequeuing a `pw_buffer` past the
//! `process` callback (so a DMA-BUF or `MemFd` frame can be handed out
//! while the producer leaves it alone), re-queuing it later, and reading
//! the metas the `pipewire` crate has no safe accessor for (sync
//! timeline, video transform, cursor).
//!
//! Every pointer here comes from `pw_stream_dequeue_buffer` on the loop
//! thread and is read only on that thread while the buffer is dequeued.
//! Nothing raw leaves this module: callers get plain values, borrowed
//! fds tied to a [`Buffer`] borrow, and [`BufferKey`]s (an address used
//! as an identity, never dereferenced).

#![allow(
    unsafe_code,
    reason = "pw_buffer / spa_buffer access: the PipeWire FFI this module wraps"
)]

use std::os::fd::{BorrowedFd, RawFd};
use std::ptr::NonNull;

use pipewire as pw;
use pw::spa::sys::{
    spa_buffer, spa_data, spa_meta, spa_meta_cursor, spa_meta_header, spa_meta_region,
};
use pw::stream::Stream;

use crate::types::{CursorState, Point, Rect};

use super::super::syncobj::{ExplicitSync, TimelineFds};

// `spa/buffer/meta.h` and `buffer.h` ids, kept here rather than taken from
// the bindings, which follow whatever headers the build machine has
// (VideoTransform and SyncTimeline are PipeWire 1.2).
pub(super) const META_HEADER: u32 = 1;
pub(super) const META_VIDEO_CROP: u32 = 2;
pub(super) const META_VIDEO_DAMAGE: u32 = 3;
pub(super) const META_CURSOR: u32 = 5;
pub(super) const META_VIDEO_TRANSFORM: u32 = 8;
pub(super) const META_SYNC_TIMELINE: u32 = 9;
pub(super) const DATA_MEM_FD: u32 = 2;
pub(super) const DATA_DMA_BUF: u32 = 3;
pub(super) const DATA_SYNC_OBJ: u32 = 5;
/// `SPA_META_SYNC_TIMELINE_UNSCHEDULED_RELEASE`: set by the producer when
/// it queues a buffer, cleared by the consumer once it scheduled the
/// release point. A producer that finds it still set does not wait on
/// the release point.
pub(super) const SYNC_TIMELINE_UNSCHEDULED_RELEASE: u32 = 1 << 0;

/// `struct spa_meta_sync_timeline`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(super) struct MetaSyncTimeline {
    flags: u32,
    padding: u32,
    acquire_point: u64,
    release_point: u64,
}

/// A dequeued buffer's identity: its address, compared, never followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct BufferKey(usize);

impl BufferKey {
    /// The key of a buffer `add_buffer` / `remove_buffer` reported.
    pub(super) fn of(buffer: *mut pw::sys::pw_buffer) -> Option<Self> {
        NonNull::new(buffer).map(|b| Self(b.as_ptr().addr()))
    }
}

/// A buffer dequeued from a stream. It goes back with
/// [`Buffer::requeue`], which also hands its explicit-sync release point
/// back; dropping it without that keeps it dequeued until the stream is
/// destroyed (a screenshot keeps its frame stable that way). It never
/// leaves the loop thread (`!Send`: raw pointer).
#[derive(Debug)]
pub(super) struct Buffer {
    raw: NonNull<pw::sys::pw_buffer>,
}

/// The acquire / release timeline of an explicitly synchronised buffer.
#[derive(Debug, Clone, Copy)]
pub(super) struct Timeline<'b> {
    pub(super) fds: TimelineFds<'b>,
    pub(super) acquire_point: u64,
    pub(super) release_point: u64,
    /// The producer set [`SYNC_TIMELINE_UNSCHEDULED_RELEASE`].
    pub(super) unscheduled_release: bool,
}

/// One data plane, as the producer described it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Plane<'b> {
    pub(super) kind: u32,
    pub(super) fd: Option<BorrowedFd<'b>>,
    pub(super) mapoffset: u32,
    pub(super) maxsize: u32,
    pub(super) chunk_offset: u32,
    pub(super) chunk_size: u32,
    pub(super) chunk_stride: i32,
}

impl Buffer {
    /// The next ready buffer of `stream`, if any.
    pub(super) fn dequeue(stream: &Stream) -> Option<Self> {
        // SAFETY: `pw_stream_dequeue_buffer` on the stream's own loop
        // thread; the result is NULL or a buffer now owned by the caller
        // until queued back (`requeue`) or the stream is destroyed.
        let raw = unsafe { stream.dequeue_raw_buffer() };
        NonNull::new(raw).map(|raw| Self { raw })
    }

    /// Hand the buffer back to the producer, its release point signalled
    /// first when it carries a timeline (whether it was read or not: a
    /// producer that waits on the point would wait forever otherwise).
    pub(super) fn requeue(self, stream: &Stream, sync: &mut ExplicitSync<BufferKey>) {
        self.settle_release(sync);
        // SAFETY: `raw` was dequeued from `stream` (the caller passes the
        // stream it dequeued from) and, `self` being consumed, is queued
        // back exactly once.
        unsafe { stream.queue_raw_buffer(self.raw.as_ptr()) };
    }

    /// Signal the release point, and once it is, clear the producer's
    /// "unscheduled" flag. When signalling fails the flag stays set, so a
    /// producer that reads it reuses the buffer without waiting.
    fn settle_release(&self, sync: &mut ExplicitSync<BufferKey>) {
        let Some(timeline) = self.timeline() else {
            return;
        };
        let (unscheduled, point) = (timeline.unscheduled_release, timeline.release_point);
        match sync.release(self.key(), timeline.fds, point) {
            Ok(()) if unscheduled => self.mark_release_scheduled(),
            Ok(()) => {}
            Err(err) => {
                tracing::warn!(%err, point, "screen-capture portal: release point not signalled");
            }
        }
    }

    /// Clear [`SYNC_TIMELINE_UNSCHEDULED_RELEASE`] in the buffer's timeline
    /// meta.
    fn mark_release_scheduled(&self) {
        let Some(data) = self.meta_data(META_SYNC_TIMELINE, size_of::<MetaSyncTimeline>()) else {
            return;
        };
        let flags = data.cast::<u32>();
        // SAFETY: the meta holds a whole `spa_meta_sync_timeline` (checked
        // by `meta_data`), whose first field is the `u32` flags; the
        // consumer owns the dequeued buffer's metas, which PipeWire maps
        // writable for it. Unaligned access: shared memory.
        let current = unsafe { flags.read_unaligned() };
        // SAFETY: as above.
        unsafe { flags.write_unaligned(current & !SYNC_TIMELINE_UNSCHEDULED_RELEASE) };
    }

    pub(super) fn key(&self) -> BufferKey {
        BufferKey(self.raw.as_ptr().addr())
    }

    const fn spa(&self) -> Option<&spa_buffer> {
        // SAFETY: `raw` is a live dequeued `pw_buffer`; its `buffer`
        // field is NULL or a `spa_buffer` that lives as long as it.
        let spa = unsafe { self.raw.as_ref().buffer };
        // SAFETY: as above; the borrow is tied to `self`.
        unsafe { spa.as_ref() }
    }

    fn metas(&self) -> &[spa_meta] {
        let Some(spa) = self.spa() else {
            return &[];
        };
        if spa.metas.is_null() {
            return &[];
        }
        let n = usize::try_from(spa.n_metas).unwrap_or(0);
        // SAFETY: PipeWire allocated `n_metas` metas at `metas`, alive
        // while the buffer is dequeued.
        unsafe { std::slice::from_raw_parts(spa.metas, n) }
    }

    fn datas(&self) -> &[spa_data] {
        let Some(spa) = self.spa() else {
            return &[];
        };
        if spa.datas.is_null() {
            return &[];
        }
        let n = usize::try_from(spa.n_datas).unwrap_or(0);
        // SAFETY: `n_datas` data blocks at `datas`, alive while dequeued.
        unsafe { std::slice::from_raw_parts(spa.datas, n) }
    }

    /// The data of the first meta of `kind` holding at least `size` bytes.
    fn meta_data(&self, kind: u32, size: usize) -> Option<*mut std::ffi::c_void> {
        self.metas()
            .iter()
            .find(|m| m.type_ == kind && !m.data.is_null() && m.size as usize >= size)
            .map(|m| m.data)
    }

    /// The first meta of `kind` holding at least `T`, read unaligned.
    fn meta<T: Copy>(&self, kind: u32) -> Option<T> {
        let data = self.meta_data(kind, size_of::<T>())?;
        // SAFETY: the meta's data holds at least `size_of::<T>()` bytes
        // (checked by `meta_data`) of a plain-data C struct; shared memory
        // may be unaligned, hence `read_unaligned`.
        Some(unsafe { data.cast::<T>().read_unaligned() })
    }

    /// Every element of the array meta of `kind` (e.g. damage regions).
    fn meta_array<T: Copy>(&self, kind: u32) -> Vec<T> {
        let Some(meta) = self
            .metas()
            .iter()
            .find(|m| m.type_ == kind && !m.data.is_null())
        else {
            return Vec::new();
        };
        let count = meta.size as usize / size_of::<T>();
        (0..count)
            .map(|i| {
                // SAFETY: element `i < size / size_of::<T>()` starts inside
                // the meta's `size` bytes.
                let element = unsafe { meta.data.cast::<T>().add(i) };
                // SAFETY: the whole element lies inside the meta (as
                // above); read unaligned (shared memory).
                unsafe { element.read_unaligned() }
            })
            .collect()
    }

    /// `spa_meta_header.pts`, when present and non-negative.
    pub(super) fn pts(&self) -> Option<u64> {
        self.meta::<spa_meta_header>(META_HEADER)
            .and_then(|h| u64::try_from(h.pts).ok())
    }

    /// `SPA_META_VideoCrop`, when present and non-empty.
    pub(super) fn crop(&self) -> Option<Rect> {
        self.meta::<spa_meta_region>(META_VIDEO_CROP)
            .map(region)
            .filter(|r| !r.is_empty())
    }

    /// `SPA_META_VideoDamage` regions (the list ends at the first empty
    /// one); `None` without the meta.
    pub(super) fn damage(&self) -> Option<Vec<Rect>> {
        if !self.metas().iter().any(|m| m.type_ == META_VIDEO_DAMAGE) {
            return None;
        }
        Some(
            self.meta_array::<spa_meta_region>(META_VIDEO_DAMAGE)
                .into_iter()
                .map(region)
                .take_while(|r| !r.is_empty())
                .collect(),
        )
    }

    /// `SPA_META_VideoTransform` (a `wl_output` transform value).
    pub(super) fn transform(&self) -> Option<u32> {
        self.meta::<u32>(META_VIDEO_TRANSFORM)
    }

    /// `SPA_META_Cursor`: position and hotspot, `None` without the meta
    /// or without cursor data (id 0).
    pub(super) fn cursor(&self) -> Option<CursorState> {
        let cursor = self.meta::<spa_meta_cursor>(META_CURSOR)?;
        if cursor.id == 0 {
            return None;
        }
        Some(CursorState {
            position: Point {
                x: cursor.position.x,
                y: cursor.position.y,
            },
            hotspot: Point {
                x: cursor.hotspot.x,
                y: cursor.hotspot.y,
            },
            visible: true,
        })
    }

    /// The explicit-sync timeline, when the producer negotiated it.
    pub(super) fn timeline(&self) -> Option<Timeline<'_>> {
        let meta = self.meta::<MetaSyncTimeline>(META_SYNC_TIMELINE)?;
        let mut syncobjs = self
            .datas()
            .iter()
            .filter(|d| d.type_ == DATA_SYNC_OBJ)
            .filter_map(|d| borrowed(d.fd));
        let (acquire, release) = (syncobjs.next()?, syncobjs.next()?);
        Some(Timeline {
            fds: TimelineFds { acquire, release },
            acquire_point: meta.acquire_point,
            release_point: meta.release_point,
            unscheduled_release: meta.flags & SYNC_TIMELINE_UNSCHEDULED_RELEASE != 0,
        })
    }

    /// The picture planes (sync objects left out).
    pub(super) fn planes(&self) -> Vec<Plane<'_>> {
        self.datas()
            .iter()
            .filter(|d| d.type_ != DATA_SYNC_OBJ)
            .map(|d| {
                // SAFETY: `chunk` is NULL or the data block's chunk, alive
                // while the buffer is dequeued.
                let chunk = unsafe { d.chunk.as_ref() };
                Plane {
                    kind: d.type_,
                    fd: borrowed(d.fd),
                    mapoffset: d.mapoffset,
                    maxsize: d.maxsize,
                    chunk_offset: chunk.map_or(0, |c| c.offset),
                    chunk_size: chunk.map_or(0, |c| c.size),
                    chunk_stride: chunk.map_or(0, |c| c.stride),
                }
            })
            .collect()
    }
}

const fn region(meta: spa_meta_region) -> Rect {
    let r = meta.region;
    Rect::new(r.position.x, r.position.y, r.size.width, r.size.height)
}

/// A data block's fd, borrowed for as long as the buffer is dequeued.
fn borrowed<'b>(fd: i64) -> Option<BorrowedFd<'b>> {
    let fd = RawFd::try_from(fd).ok().filter(|fd| *fd >= 0)?;
    // SAFETY: PipeWire owns the descriptors of a buffer's data blocks and
    // keeps them open while the buffer exists; callers only use the
    // borrow while the buffer is dequeued (it is tied to a `&Buffer`).
    Some(unsafe { BorrowedFd::borrow_raw(fd) })
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
    use std::time::Duration;

    use super::*;
    use crate::desktop::linux::syncobj::{SyncDevice, Waited};

    /// A `pw_buffer` as a producer hands it over: a timeline meta and the
    /// acquire / release syncobj data blocks, in memory the test owns.
    struct FakeBuffer {
        timeline: Box<MetaSyncTimeline>,
        metas: Box<[spa_meta; 1]>,
        datas: Box<[spa_data; 2]>,
        spa: Box<spa_buffer>,
        pw: Box<pw::sys::pw_buffer>,
    }

    impl FakeBuffer {
        fn new(acquire: &OwnedFd, release: &OwnedFd, release_point: u64, flags: u32) -> Self {
            // SAFETY: plain C structs, all-zero is a valid (empty) value.
            let zeroed_data = || unsafe { std::mem::zeroed::<spa_data>() };
            let mut this = Self {
                timeline: Box::new(MetaSyncTimeline {
                    flags,
                    padding: 0,
                    acquire_point: 1,
                    release_point,
                }),
                // SAFETY: as above.
                metas: Box::new([unsafe { std::mem::zeroed() }]),
                datas: Box::new([zeroed_data(), zeroed_data()]),
                // SAFETY: as above.
                spa: Box::new(unsafe { std::mem::zeroed() }),
                // SAFETY: as above.
                pw: Box::new(unsafe { std::mem::zeroed() }),
            };
            this.metas[0].type_ = META_SYNC_TIMELINE;
            this.metas[0].size = u32::try_from(size_of::<MetaSyncTimeline>()).expect("small");
            this.metas[0].data = std::ptr::from_mut(&mut *this.timeline).cast();
            for (data, fd) in this.datas.iter_mut().zip([acquire, release]) {
                data.type_ = DATA_SYNC_OBJ;
                data.fd = i64::from(fd.as_raw_fd());
            }
            this.spa.n_metas = 1;
            this.spa.metas = this.metas.as_mut_ptr();
            this.spa.n_datas = 2;
            this.spa.datas = this.datas.as_mut_ptr();
            this.pw.buffer = std::ptr::from_mut(&mut *this.spa);
            this
        }

        fn buffer(&mut self) -> Buffer {
            Buffer {
                raw: NonNull::from(&mut *self.pw),
            }
        }

        fn flags(&self) -> u32 {
            // SAFETY: the meta points at `self.timeline`, alive here; the
            // flags are its first field. Read through the pointer the
            // buffer wrote through.
            unsafe { self.metas[0].data.cast::<u32>().read() }
        }
    }

    #[test]
    fn a_requeued_timeline_buffer_signals_its_release_point_and_clears_the_flag() {
        let Some(device) = SyncDevice::open(None).ok() else {
            eprintln!("skipping: no render node");
            return;
        };
        let (Ok(acquire), Ok(release)) = (device.create(), device.create()) else {
            eprintln!("skipping: no timeline syncobjs on this node");
            return;
        };
        let (a, r) = (acquire.export().expect("a"), release.export().expect("r"));
        let mut fake = FakeBuffer::new(&a, &r, 7, SYNC_TIMELINE_UNSCHEDULED_RELEASE | 1 << 4);
        let mut sync = ExplicitSync::new(None);
        // Never read (an older buffer `newest` skipped, an acquire timeout):
        // still released.
        fake.buffer().settle_release(&mut sync);
        assert_eq!(
            release.wait(7, Duration::from_millis(100)).expect("wait"),
            Waited::Signalled,
            "a producer waiting on the release point is not stuck"
        );
        assert_eq!(fake.flags(), 1 << 4, "only the unscheduled flag is cleared");
    }

    #[test]
    fn an_unsignalled_release_point_keeps_the_unscheduled_flag() {
        // A memfd is no syncobj: the import (or the device) fails.
        let not_syncobj = rustix::fs::memfd_create("not-syncobj", rustix::fs::MemfdFlags::CLOEXEC)
            .expect("memfd");
        let other = rustix::io::fcntl_dupfd_cloexec(not_syncobj.as_fd(), 0).expect("dup");
        let mut fake = FakeBuffer::new(&not_syncobj, &other, 3, SYNC_TIMELINE_UNSCHEDULED_RELEASE);
        fake.buffer().settle_release(&mut ExplicitSync::new(None));
        assert_eq!(
            fake.flags(),
            SYNC_TIMELINE_UNSCHEDULED_RELEASE,
            "the producer must not wait on a point nobody signals"
        );
    }
}
