//! The PipeWire video streams of a portal cast: one per chosen source,
//! all on one loop thread. A screenshot takes the first frame of each
//! and ends; a session delivers frames into its [`FrameOut`].
//!
//! Frames are copied out of the shared-memory buffer while it is
//! dequeued, so the buffer goes straight back to the producer; the copy
//! sits in a plugin pool slot accounted by [`PoolAccounting`] (never the
//! last free one), which is what `Exhaustion` applies to.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use istmo_core::NativeBuffer;
use pipewire as pw;
use pw::properties::properties;
use pw::spa::buffer::DataType;
use pw::spa::buffer::meta::{MetaHeader, MetaVideoCrop, MetaVideoDamage};
use pw::spa::param::ParamType;
use pw::spa::param::format::{MediaSubtype, MediaType};
use pw::spa::param::format_utils;
use pw::spa::param::video::VideoInfoRaw;
use pw::spa::utils::Direction;
use pw::stream::{Stream, StreamFlags, StreamRc};

use crate::pool::PoolAccounting;
use crate::types::{
    BackendId, BufferIdentity, BufferMemory, CaptureError, Colour, CopyStage, Damage, Exhaustion,
    FrameMeta, Orientation, PixelFormat, PlaneLayout, PtsClock, Rect, Size, Source, SyncKind,
};

use super::super::super::backend::Captured;
use super::super::super::clock::monotonic_ns;
use super::super::super::output::{Delivery, FrameOut};
use super::super::pipewire::{self as pwx, LoopCtx, LoopThread, Remote, StreamEnd};
use super::pods::{self, ShmTier};

const ID: Option<BackendId> = Some(BackendId::LinuxPortal);

fn error(step: &str, message: impl std::fmt::Display) -> CaptureError {
    pwx::error(ID, step, message)
}

/// How long a screenshot waits for every stream's first frame.
const SHOT_TIMEOUT: Duration = Duration::from_secs(10);

/// One stream to open: the portal's node and what it shows.
#[derive(Debug, Clone)]
pub(super) struct Part {
    pub(super) node: u32,
    pub(super) source: Source,
}

/// What negotiation settled on for a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Negotiated {
    pub(super) format: PixelFormat,
    pub(super) size: Size,
    pub(super) tier: u8,
    pub(super) rate_mhz: Option<u32>,
}

/// Where a live session's frames go.
#[derive(Debug)]
pub(super) struct Live {
    pub(super) out: FrameOut,
    pub(super) pool_depth: u8,
    pub(super) exhaustion: Exhaustion,
}

/// Frame rows of a CPU-readable plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RowLayout {
    /// Bytes between the starts of consecutive rows.
    stride: u32,
    /// Bytes of pixels in one row (`<= stride` when valid).
    row_bytes: u32,
    height: u32,
}

impl RowLayout {
    /// The bytes of the frame inside a mapping of `len` bytes, starting
    /// at `offset`: `height` full strides when they fit, else a last row
    /// trimmed to `row_bytes`. `None` when the stride cannot hold a row
    /// or the mapping is short.
    fn frame_range(self, len: usize, offset: u32) -> Option<std::ops::Range<usize>> {
        let (stride, row_bytes) = (self.stride as usize, self.row_bytes as usize);
        if self.height == 0 || row_bytes == 0 || stride < row_bytes {
            return None;
        }
        let start = offset as usize;
        let full = stride.checked_mul(self.height as usize)?;
        let min = full.checked_sub(stride)?.checked_add(row_bytes)?;
        let available = len.checked_sub(start)?;
        if available < min {
            return None;
        }
        Some(start..start + full.min(available))
    }
}

/// What both modes report to the caller.
enum Report {
    Negotiated(Negotiated),
    Shots(Vec<Captured>),
}

/// State shared by the streams of one loop.
struct Shared {
    ready: Option<flume::Sender<Result<Report, CaptureError>>>,
    mainloop: pw::main_loop::MainLoopRc,
    shots: Vec<Option<Captured>>,
}

impl Shared {
    fn report(&mut self, result: Result<Report, CaptureError>) {
        if let Some(ready) = self.ready.take() {
            // The caller gave up waiting when this fails.
            let _unwanted = ready.send(result);
        }
    }
}

enum Mode {
    Shot,
    Live {
        live: Live,
        pool: PoolAccounting,
        wake: pw::channel::Sender<Wake>,
    },
}

/// Sent to the loop when a pool slot came back (a stalled session pulls
/// again).
struct Wake;

/// One stream's state, on the loop thread.
struct StreamData {
    index: usize,
    source: Source,
    tiers: Vec<ShmTier>,
    info: VideoInfoRaw,
    negotiated: Option<Negotiated>,
    shared: Rc<RefCell<Shared>>,
    mode: Mode,
    /// Frames dropped since the last delivered one.
    skipped: u32,
    slot: u32,
}

/// One frame per part. Returns the frames in part order.
///
/// # Errors
/// PipeWire failures, no frame within [`SHOT_TIMEOUT`].
pub(super) fn screenshot(
    remote: Remote,
    parts: Vec<Part>,
    tiers: Vec<ShmTier>,
) -> Result<Vec<Captured>, CaptureError> {
    let (_thread, report) = LoopThread::spawn(
        ID,
        "istmo-screen-capture-portal",
        remote,
        SHOT_TIMEOUT,
        move |ctx, ready| run(ctx, ready, parts, &tiers, None),
    )?;
    match report {
        Report::Shots(frames) => Ok(frames),
        Report::Negotiated(_) => Err(error("screenshot", "the stream reported no frame")),
    }
}

/// A live stream on `part`, delivering into `live.out`.
///
/// # Errors
/// PipeWire failures, no negotiation within the setup timeout.
pub(super) fn open(
    remote: Remote,
    part: Part,
    tiers: Vec<ShmTier>,
    live: Live,
) -> Result<(LoopThread, Negotiated), CaptureError> {
    let (thread, report) = LoopThread::spawn(
        ID,
        "istmo-screen-capture-portal",
        remote,
        pwx::SETUP_TIMEOUT,
        move |ctx, ready| run(ctx, ready, vec![part], &tiers, Some(live)),
    )?;
    match report {
        Report::Negotiated(negotiated) => Ok((thread, negotiated)),
        Report::Shots(_) => Err(error(
            "open",
            "the stream reported frames instead of a format",
        )),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one setup sequence of the loop thread: streams, listeners, wake-ups, then the loop"
)]
fn run(
    ctx: LoopCtx,
    ready: &flume::Sender<Result<Report, CaptureError>>,
    parts: Vec<Part>,
    tiers: &[ShmTier],
    live: Option<Live>,
) {
    let fail = |err| {
        // The caller gave up waiting when this fails.
        let _unwanted = ready.send(Err(err));
    };
    let enum_formats = match pods::enum_formats(
        tiers,
        parts.first().and_then(|p| p.source.physical_size),
        60,
    ) {
        Ok(pods) => pods,
        Err(err) => {
            fail(err);
            return;
        }
    };
    let shared = Rc::new(RefCell::new(Shared {
        ready: Some(ready.clone()),
        mainloop: ctx.mainloop.clone(),
        shots: parts.iter().map(|_| None).collect(),
    }));
    let (wake_tx, wake_rx) = pw::channel::channel::<Wake>();
    let mut live = live;
    let mut streams = Vec::new();
    let mut listeners = Vec::new();
    for (index, part) in parts.into_iter().enumerate() {
        let stream = match StreamRc::new(
            ctx.core.clone(),
            "istmo-screen-capture",
            properties! {
                *pw::keys::MEDIA_TYPE => "Video",
                *pw::keys::MEDIA_CATEGORY => "Capture",
                *pw::keys::MEDIA_ROLE => "Screen",
            },
        ) {
            Ok(stream) => stream,
            Err(err) => {
                fail(error("create the stream", err));
                return;
            }
        };
        let mode = match live.take() {
            Some(live) => Mode::Live {
                pool: PoolAccounting::new(usize::from(live.pool_depth)),
                live,
                wake: wake_tx.clone(),
            },
            None => Mode::Shot,
        };
        let data = Rc::new(RefCell::new(StreamData {
            index,
            source: part.source,
            tiers: tiers.to_vec(),
            info: VideoInfoRaw::default(),
            negotiated: None,
            shared: Rc::clone(&shared),
            mode,
            skipped: 0,
            slot: 0,
        }));
        let listener = stream
            .add_local_listener_with_user_data(Rc::clone(&data))
            .state_changed(|_, data, old, new| {
                let Some(end) = StreamEnd::of(&old, &new) else {
                    return;
                };
                on_end(&data.borrow(), end_error(end));
            })
            .param_changed(|stream, data, id, param| {
                on_param(stream, &mut data.borrow_mut(), id, param);
            })
            .process(|stream, data| on_process(stream, &mut data.borrow_mut()))
            .register();
        match listener {
            Ok(listener) => listeners.push(listener),
            Err(err) => {
                fail(error("register the stream listener", err));
                return;
            }
        }
        let connected = enum_formats
            .iter()
            .map(|bytes| pwx::pod(ID, bytes))
            .collect::<Result<Vec<_>, _>>()
            .and_then(|mut pods| {
                stream
                    .connect(
                        Direction::Input,
                        Some(part.node),
                        // DONT_RECONNECT: when the cast ends ("Stop
                        // sharing", the window closed) the stream must
                        // not be relinked to another source; it falls to
                        // `Unconnected`, which ends the session.
                        StreamFlags::AUTOCONNECT
                            | StreamFlags::MAP_BUFFERS
                            | StreamFlags::DONT_RECONNECT,
                        &mut pods,
                    )
                    .map_err(|err| error("connect the stream", err))
            });
        if let Err(err) = connected {
            fail(err);
            return;
        }
        streams.push((stream, data));
    }
    // A pool slot came back: let a stalled stream pull its newest frame.
    let mainloop = ctx.mainloop.clone();
    let _wakes = wake_rx.attach(mainloop.loop_(), {
        let streams: Vec<_> = streams
            .iter()
            .map(|(s, d)| (s.clone(), Rc::clone(d)))
            .collect();
        move |Wake| {
            for (stream, data) in &streams {
                on_process(stream, &mut data.borrow_mut());
            }
        }
    });
    drop(wake_tx);
    ctx.run();
    for (_, data) in &streams {
        if let Mode::Live { live, .. } = &data.borrow().mode {
            live.out.stop(crate::types::StopReason::Closed);
        }
    }
    drop(listeners);
}

/// PipeWire's "no more input formats": the producer offers none of the
/// formats (and memory) the stream listed.
const NO_FORMAT: &str = "no more input formats";

/// The session error of a stream end. A producer that offers none of the
/// shared-memory formats (niri's offers DMA-BUF only) is a format the
/// backend cannot serve, not a protocol failure.
fn end_error(end: StreamEnd) -> CaptureError {
    match end {
        StreamEnd::Error(message) if message.contains(NO_FORMAT) => {
            CaptureError::unsatisfiable(ID, crate::types::Requirement::Formats)
        }
        other => other.into_error(ID),
    }
}

fn on_end(data: &StreamData, err: CaptureError) {
    match &data.mode {
        Mode::Live { live, .. } => live.out.fail(err.clone()),
        Mode::Shot => {}
    }
    let mut shared = data.shared.borrow_mut();
    shared.report(Err(err));
    shared.mainloop.quit();
}

fn on_param(stream: &Stream, data: &mut StreamData, id: u32, param: Option<&pw::spa::pod::Pod>) {
    let Some(param) = param else { return };
    if id != ParamType::Format.as_raw() {
        return;
    }
    let Ok((MediaType::Video, MediaSubtype::Raw)) = format_utils::parse_format(param) else {
        return;
    };
    if let Err(err) = data.info.parse(param) {
        tracing::warn!(?err, "screen-capture portal: unreadable video format");
        return;
    }
    let Some(format) = pods::pixel_format(data.info.format()) else {
        tracing::warn!(format = ?data.info.format(), "screen-capture portal: unexpected video format");
        return;
    };
    let Some(tier) = pods::tier_of(&data.tiers, format) else {
        return;
    };
    let size = data.info.size();
    let rate = data.info.framerate();
    let negotiated = Negotiated {
        format,
        size: Size::new(size.width, size.height),
        tier,
        rate_mhz: (rate.denom > 0 && rate.num > 0).then(|| {
            u32::try_from(u64::from(rate.num) * 1000 / u64::from(rate.denom)).unwrap_or(u32::MAX)
        }),
    };
    let reallocated = data.negotiated.is_some_and(|old| old != negotiated);
    data.negotiated = Some(negotiated);
    if let Mode::Live { pool, live, .. } = &data.mode {
        if reallocated {
            let _generation = pool.reallocate(usize::from(live.pool_depth));
            live.out
                .event(crate::types::CaptureEvent::SourceResized(negotiated.size));
        }
        data.shared
            .borrow_mut()
            .report(Ok(Report::Negotiated(negotiated)));
    }
    let depth = match &data.mode {
        Mode::Live { live, .. } => live.pool_depth,
        Mode::Shot => 2,
    };
    let params =
        pods::buffers(depth).and_then(|buffers| pods::metas().map(|metas| (buffers, metas)));
    let sent = params.and_then(|(buffers, metas)| {
        let mut pods = vec![pwx::pod(ID, &buffers)?];
        for meta in &metas {
            pods.push(pwx::pod(ID, meta)?);
        }
        stream
            .update_params(&mut pods)
            .map_err(|err| error("update_params(Buffers, Meta)", err))
    });
    if let Err(err) = sent {
        on_end(data, err);
    }
}

/// The newest dequeued buffer copied out (the others go straight back).
struct Copied {
    bytes: Vec<u8>,
    stride: u32,
    content: Rect,
    damage: Damage,
    pts_ns: u64,
}

/// Drop every queued buffer; how many there were.
fn drain(stream: &Stream) -> u32 {
    let mut dropped = 0;
    while stream.dequeue_buffer().is_some() {
        dropped += 1;
    }
    dropped
}

/// Dequeue everything ready, keep the newest, copy it out.
fn take_newest(stream: &Stream, negotiated: Negotiated, skipped: &mut u32) -> Option<Copied> {
    let mut newest = None;
    while let Some(buffer) = stream.dequeue_buffer() {
        // Replacing re-queues the older one.
        if newest.replace(buffer).is_some() {
            *skipped = skipped.saturating_add(1);
        }
    }
    let mut buffer = newest?;
    let pts_ns = buffer
        .find_meta::<MetaHeader>()
        .and_then(|header| u64::try_from(header.pts()).ok())
        .filter(|pts| plausible_monotonic(*pts))
        .unwrap_or_else(monotonic_ns);
    let bounds = Rect::from_size(negotiated.size);
    let content = buffer
        .find_meta::<MetaVideoCrop>()
        .map(|crop| {
            let region = crop.meta_region();
            Rect::new(
                region.position().x,
                region.position().y,
                region.size().width,
                region.size().height,
            )
        })
        .filter(|rect| !rect.is_empty())
        .and_then(|rect| rect.intersect(bounds))
        .unwrap_or(bounds);
    let damage = buffer
        .find_meta::<MetaVideoDamage>()
        .map_or(Damage::Full, |meta| {
            let rects: Vec<Rect> = meta
                .iter()
                .filter_map(|region| {
                    Rect::new(
                        region.position().x,
                        region.position().y,
                        region.size().width,
                        region.size().height,
                    )
                    .intersect(bounds)
                })
                .collect();
            if rects.is_empty() {
                Damage::Full
            } else {
                Damage::Rects(rects)
            }
        });
    let plane = buffer.datas_mut().first_mut()?;
    if !matches!(plane.type_(), DataType::MemFd | DataType::MemPtr) {
        return None;
    }
    let chunk = plane.chunk();
    let (offset, size) = (chunk.offset(), chunk.size());
    let row_bytes = negotiated
        .size
        .width
        .saturating_mul(negotiated.format.bytes_per_pixel());
    let stride = u32::try_from(chunk.stride())
        .ok()
        .filter(|s| *s > 0)
        .unwrap_or(row_bytes);
    if size == 0 {
        // The producer has not rendered into it yet.
        return None;
    }
    let layout = RowLayout {
        stride,
        row_bytes,
        height: negotiated.size.height,
    };
    let mapped = plane.data()?;
    let range = layout.frame_range(mapped.len(), offset)?;
    let mut bytes = mapped.get(range)?.to_vec();
    // A trimmed last row is padded back to a full stride, so every row
    // of the copy is `stride` bytes.
    bytes.resize(stride as usize * negotiated.size.height as usize, 0);
    Some(Copied {
        bytes,
        stride,
        content,
        damage,
        pts_ns,
    })
}

/// A header pts within a minute of `CLOCK_MONOTONIC` now is on that
/// clock; anything else is a producer-specific clock and is replaced by
/// the arrival time.
fn plausible_monotonic(pts: u64) -> bool {
    const WINDOW_NS: u64 = 60_000_000_000;
    monotonic_ns().abs_diff(pts) < WINDOW_NS
}

fn meta_for(
    data: &StreamData,
    negotiated: Negotiated,
    copied: &Copied,
    buffer: BufferIdentity,
) -> FrameMeta {
    FrameMeta {
        seq: 0,
        pts_ns: copied.pts_ns,
        pts_clock: PtsClock::Monotonic,
        wall_us: 0,
        size: negotiated.size,
        content_rect: copied.content,
        format: negotiated.format,
        memory: BufferMemory::Cpu,
        planes: vec![PlaneLayout {
            offset: 0,
            stride: copied.stride,
            size: copied.bytes.len() as u64,
        }],
        modifier: None,
        buffer,
        tier: negotiated.tier,
        copied: None,
        producer: None,
        colour: Colour::SRGB,
        hdr: None,
        // The video-transform meta is not readable through the safe API;
        // portals of rotated outputs are reported `Unsupported`.
        orientation: Orientation::UPRIGHT,
        damage: copied.damage.clone(),
        cursor: None,
        sync: SyncKind::Ready,
        scale_permille: data.source.scale(),
        source_id: data.source.id.clone(),
        repeat: false,
        skipped_before: data.skipped,
        protected_content: false,
        composite: None,
    }
}

fn on_process(stream: &Stream, data: &mut StreamData) {
    let Some(negotiated) = data.negotiated else {
        drain(stream);
        return;
    };
    match &data.mode {
        Mode::Shot => shot(stream, data, negotiated),
        Mode::Live { .. } => deliver(stream, data, negotiated),
    }
}

fn shot(stream: &Stream, data: &StreamData, negotiated: Negotiated) {
    let done = data
        .shared
        .borrow()
        .shots
        .get(data.index)
        .is_none_or(Option::is_some);
    if done {
        drain(stream);
        return;
    }
    let mut skipped = 0;
    let Some(copied) = take_newest(stream, negotiated, &mut skipped) else {
        return;
    };
    let meta = meta_for(data, negotiated, &copied, BufferIdentity::default());
    let mut source = data.source.clone();
    source.physical_size = Some(negotiated.size);
    if let Some(geometry) = source.geometry.filter(|g| g.width > 0) {
        source.scale_permille = Some(negotiated.size.width.saturating_mul(1000) / geometry.width);
    }
    let captured = Captured {
        meta: FrameMeta {
            scale_permille: source.scale(),
            ..meta
        },
        buffer: NativeBuffer::from_bytes(copied.bytes),
        source,
    };
    let mut shared = data.shared.borrow_mut();
    if let Some(slot) = shared.shots.get_mut(data.index) {
        *slot = Some(captured);
    }
    if shared.shots.iter().all(Option::is_some) {
        let frames = shared.shots.iter_mut().filter_map(Option::take).collect();
        shared.report(Ok(Report::Shots(frames)));
        shared.mainloop.quit();
    }
}

fn deliver(stream: &Stream, data: &mut StreamData, negotiated: Negotiated) {
    let Mode::Live { live, pool, wake } = &data.mode else {
        return;
    };
    let (out, pool, wake) = (live.out.clone(), pool.clone(), wake.clone());
    let (exhaustion, pool_depth) = (live.exhaustion, live.pool_depth);
    if out.is_stopped() {
        drain(stream);
        data.shared.borrow().mainloop.quit();
        return;
    }
    if !out.wants_frames() {
        drain(stream);
        return;
    }
    // Stall: leave the buffers queued in PipeWire (the producer stops for
    // want of buffers) until a slot comes back and wakes the loop.
    if exhaustion == Exhaustion::Stall && !pool.has_room() {
        if let Err(exhausted) = pool.hold(data.slot) {
            out.exhausted(exhausted, pool_depth, exhaustion);
        }
        return;
    }
    let mut skipped = data.skipped;
    let Some(copied) = take_newest(stream, negotiated, &mut skipped) else {
        data.skipped = skipped;
        return;
    };
    data.skipped = skipped;
    let slot = data.slot % u32::from(pool_depth.max(1));
    let mut meta = meta_for(data, negotiated, &copied, BufferIdentity::default());
    let bytes = copied.bytes;
    let (buffer, identity, copy) = match pool.hold(slot) {
        Ok(lease) => {
            let identity = lease.identity();
            let buffer = NativeBuffer::from_bytes(bytes)
                .with_identity(identity.into())
                .with_release(move || {
                    drop(lease);
                    // Fails only once the loop ended.
                    let _ended = wake.send(Wake);
                });
            (buffer, identity, None)
        }
        Err(exhausted) => {
            out.exhausted(exhausted, pool_depth, exhaustion);
            match exhaustion {
                Exhaustion::Stall | Exhaustion::DropNewest => {
                    data.skipped = data.skipped.saturating_add(1);
                    return;
                }
                // Already CPU memory: the frame leaves without a pool slot.
                Exhaustion::CopyToCpu => (
                    NativeBuffer::from_bytes(bytes),
                    BufferIdentity::default(),
                    Some(CopyStage::ExhaustionReadback),
                ),
            }
        }
    };
    meta.buffer = identity;
    meta.copied = copy;
    data.slot = data.slot.wrapping_add(1);
    match out.deliver(meta, buffer) {
        Delivery::Delivered => data.skipped = 0,
        Delivery::Closed => data.shared.borrow().mainloop.quit(),
        Delivery::Deferred | Delivery::Dropped => {}
    }
}

#[cfg(test)]
mod tests {
    use super::RowLayout;

    const LAYOUT: RowLayout = RowLayout {
        stride: 64,
        row_bytes: 40,
        height: 3,
    };

    #[test]
    fn a_producer_without_shared_memory_is_a_format_miss() {
        use super::{StreamEnd, end_error};
        use crate::types::{BackendId, CaptureError, Requirement};
        assert_eq!(
            end_error(StreamEnd::Error(
                "error alloc buffers: no more input formats".into()
            )),
            CaptureError::unsatisfiable(Some(BackendId::LinuxPortal), Requirement::Formats)
        );
        assert_eq!(end_error(StreamEnd::Disconnected), CaptureError::SourceLost);
    }

    #[test]
    fn frame_range_takes_full_strides_when_available() {
        assert_eq!(LAYOUT.frame_range(1024, 16), Some(16..16 + 192));
    }

    #[test]
    fn frame_range_accepts_trimmed_last_row() {
        // Two full strides and the 40 pixel bytes of the last row.
        assert_eq!(LAYOUT.frame_range(168, 0), Some(0..168));
    }

    #[test]
    fn frame_range_rejects_short_or_inconsistent_planes() {
        assert_eq!(LAYOUT.frame_range(167, 0), None);
        assert_eq!(LAYOUT.frame_range(100, 200), None);
        let narrow = RowLayout {
            stride: 32,
            ..LAYOUT
        };
        assert_eq!(narrow.frame_range(1024, 0), None);
        let empty = RowLayout {
            height: 0,
            ..LAYOUT
        };
        assert_eq!(empty.frame_range(1024, 0), None);
    }
}
