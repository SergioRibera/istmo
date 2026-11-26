//! The PipeWire video streams of a portal cast: one per chosen source,
//! all on one loop thread. A screenshot takes the first frame of each
//! and ends; a session delivers frames into its [`FrameOut`].
//!
//! Frames leave without a copy: a DMA-BUF frame carries duplicates of
//! the buffer's fds, a `MemFd` frame a mapping of its memory, and the
//! `pw_buffer` stays dequeued (the producer leaves it alone) until every
//! clone of the frame dropped. Its lease then posts the buffer back to
//! the loop thread, which signals the explicit-sync release point and
//! re-queues it. The consumer holds at most all buffers but one (the
//! producer always has one to render into): that is what `Exhaustion`
//! applies to.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use istmo_core::{DmaBuf, DmaBufPlane, NativeBuffer};
use pipewire as pw;
use pw::properties::properties;
use pw::spa::param::ParamType;
use pw::spa::param::format::{MediaSubtype, MediaType};
use pw::spa::param::format_utils;
use pw::spa::param::video::VideoInfoRaw;
use pw::spa::utils::Direction;
use pw::stream::{Stream, StreamFlags, StreamRc};

use crate::pool::Exhausted;
use crate::types::{
    BackendId, BufferIdentity, BufferMemory, CaptureError, CaptureEvent, Colour, CopyStage,
    CursorState, Damage, Exhaustion, FrameMeta, GpuIdentity, Orientation, PixelFormat, PlaneLayout,
    PtsClock, Rect, Size, Source, SyncKind,
};

use super::super::super::backend::Captured;
use super::super::super::clock::monotonic_ns;
use super::super::super::output::{Delivery, FrameOut};
use super::super::mapping::{MappedFrame, Mapping};
use super::super::pipewire::{self as pwx, LoopCtx, LoopThread, Remote, StreamEnd};
use super::super::syncobj::{Acquire, AcquireWait, ExplicitSync};
use super::pods::{self, PortalTier};
use super::raw::{self, Buffer, BufferKey};

const ID: Option<BackendId> = Some(BackendId::LinuxPortal);

// `SPA_VIDEO_FLAG_*` (`spa/param/video/raw.h`), kept here: the bindings
// gate them behind version features this crate does not raise its floor
// for.
const VIDEO_FLAG_MODIFIER: u32 = 1 << 2;
const VIDEO_FLAG_MODIFIER_FIXATION_REQUIRED: u32 = 1 << 3;

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
    /// `Some` for DMA-BUF.
    pub(super) modifier: Option<u64>,
}

impl Negotiated {
    pub(super) const fn memory(&self) -> BufferMemory {
        if self.modifier.is_some() {
            BufferMemory::DmaBuf
        } else {
            BufferMemory::Cpu
        }
    }
}

/// What the streams are opened with.
#[derive(Debug, Clone)]
pub(super) struct Setup {
    pub(super) tiers: Vec<PortalTier>,
    /// The compositor's render device (DMA-BUF producer, syncobj node).
    pub(super) producer: Option<u64>,
    /// Ask for the cursor meta (`CursorMode::Metadata`).
    pub(super) cursor_meta: bool,
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

/// Identity of one hold on a dequeued buffer, unique for the loop's life
/// so a stale release never aliases a newer hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct LeaseId(u64);

/// Posted by a frame's release hook to the loop thread: which stream,
/// which hold.
struct Release {
    stream: usize,
    id: LeaseId,
}

/// What wakes the loop thread from outside it.
enum Wake {
    Release(Release),
    /// A stream's parked buffer finished its acquire wait (`Err`: why it
    /// cannot be read).
    Acquired {
        stream: usize,
        key: BufferKey,
        result: Result<(), String>,
    },
}

/// Waits on one stream's acquire points on a thread of its own, so a
/// slow producer never stalls the other streams of the loop. The thread
/// ends with the stream's leases (its job queue closes), within one
/// acquire timeout.
struct AcquireWaiter {
    jobs: flume::Sender<(BufferKey, AcquireWait)>,
}

impl AcquireWaiter {
    fn spawn(stream: usize, wake: pw::channel::Sender<Wake>) -> std::io::Result<Self> {
        let (jobs, queue) = flume::unbounded::<(BufferKey, AcquireWait)>();
        std::thread::Builder::new()
            .name("istmo-sc-acquire".to_owned())
            .spawn(move || {
                for (key, wait) in &queue {
                    let result = wait.wait().map_err(|e| e.to_string());
                    if wake
                        .send(Wake::Acquired {
                            stream,
                            key,
                            result,
                        })
                        .is_err()
                    {
                        // The loop ended.
                        break;
                    }
                }
            })?;
        Ok(Self { jobs })
    }
}

/// A dequeued buffer waiting for its acquire point.
struct Parked {
    buffer: Buffer,
    /// `None` while the wait runs; then whether it can be read.
    acquired: Option<Result<(), String>>,
}

/// Which buffers the consumer holds, independent of PipeWire so it can
/// be unit-tested. A buffer is held only while `held < pool - 1`.
#[derive(Debug)]
struct HeldBuffers<B> {
    pool_size: usize,
    held: Vec<(LeaseId, B)>,
    next_id: u64,
}

impl<B> Default for HeldBuffers<B> {
    fn default() -> Self {
        Self {
            pool_size: 0,
            held: Vec::new(),
            next_id: 0,
        }
    }
}

impl<B: Copy + PartialEq> HeldBuffers<B> {
    const fn buffer_added(&mut self) {
        self.pool_size += 1;
    }

    /// `buffer` is about to be freed; whether it was held. Its lease, if
    /// any, becomes inert.
    fn buffer_removed(&mut self, buffer: B) -> bool {
        self.pool_size = self.pool_size.saturating_sub(1);
        let before = self.held.len();
        self.held.retain(|&(_, b)| b != buffer);
        self.held.len() != before
    }

    /// All but one: the producer always has a buffer to render into.
    const fn limit(&self) -> usize {
        self.pool_size.saturating_sub(1)
    }

    const fn has_room(&self) -> bool {
        self.held.len() < self.limit()
    }

    /// Record `buffer` as held, or `None` at the limit.
    fn hold(&mut self, buffer: B) -> Option<LeaseId> {
        if !self.has_room() {
            return None;
        }
        let id = LeaseId(self.next_id);
        self.next_id += 1;
        self.held.push((id, buffer));
        Some(id)
    }

    /// End hold `id`: the buffer to re-queue, `None` when it was removed.
    fn release(&mut self, id: LeaseId) -> Option<B> {
        let at = self.held.iter().position(|&(held, _)| held == id)?;
        Some(self.held.swap_remove(at).1)
    }
}

/// What a buffer's frames share across dequeues: its fds duplicated or
/// its memory mapped once, and its identity.
#[derive(Debug, Clone)]
enum Backing {
    DmaBuf(Arc<DmaBuf>),
    Mapped(Arc<Mapping>),
}

/// A change to a stream's buffer pool, in the order PipeWire reported it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolChange {
    Added(BufferKey),
    Removed(BufferKey),
}

/// Pool changes reported while the stream's leases were borrowed (PipeWire
/// calling back from inside a call made under that borrow), applied in
/// order before the leases are used next.
type PendingChanges = Rc<RefCell<Vec<PoolChange>>>;

/// The lease side of one stream. Separate from [`StreamData`]: PipeWire
/// adds and removes buffers from inside `update_params`, which runs
/// while the stream data is borrowed.
struct Leases {
    book: HeldBuffers<BufferKey>,
    /// Buffers held for the consumer.
    dequeued: HashMap<LeaseId, Buffer>,
    /// Acquire waits and release signals of this stream's buffers.
    sync: ExplicitSync<BufferKey>,
    /// Slot of each live buffer, for `BufferIdentity`.
    slots: Vec<Option<BufferKey>>,
    backings: HashMap<BufferKey, Backing>,
    generation: u64,
    /// A buffer was removed: the next added one starts a generation.
    removed: bool,
    tx: pw::channel::Sender<Wake>,
    /// The buffer whose acquire point is being waited for.
    parked: Option<Parked>,
    /// Started at the first acquire point that had not signalled yet.
    waiter: Option<AcquireWaiter>,
    /// This stream's index in the loop.
    stream: usize,
    /// Pool changes reported while the leases were borrowed.
    pending: PendingChanges,
    /// The current exhaustion episode (reported once).
    exhausted: bool,
    episodes: u32,
}

impl Leases {
    fn new(
        tx: pw::channel::Sender<Wake>,
        stream: usize,
        pending: PendingChanges,
        sync: ExplicitSync<BufferKey>,
    ) -> Self {
        Self {
            parked: None,
            waiter: None,
            book: HeldBuffers::default(),
            dequeued: HashMap::new(),
            sync,
            slots: Vec::new(),
            backings: HashMap::new(),
            generation: BufferIdentity::default().pool_generation + 1,
            removed: false,
            tx,
            stream,
            pending,
            exhausted: false,
            episodes: 0,
        }
    }

    /// Apply the pool changes reported while the leases were borrowed.
    fn catch_up(&mut self) {
        let changes = std::mem::take(&mut *self.pending.borrow_mut());
        for change in changes {
            match change {
                PoolChange::Added(key) => self.added(key),
                PoolChange::Removed(key) => self.removed(key),
            }
        }
    }

    /// Record `change`: applied now (after any pending ones), or queued when
    /// the leases are borrowed.
    fn report(leases: &RefCell<Self>, pending: &PendingChanges, change: PoolChange) {
        match leases.try_borrow_mut() {
            Ok(mut leases) => {
                leases.catch_up();
                match change {
                    PoolChange::Added(key) => leases.added(key),
                    PoolChange::Removed(key) => leases.removed(key),
                }
            }
            // A freed buffer must never be re-queued, and an added one
            // must count towards the pool: neither may be lost.
            Err(_) => pending.borrow_mut().push(change),
        }
    }

    fn added(&mut self, key: BufferKey) {
        if std::mem::take(&mut self.removed) {
            self.generation += 1;
        }
        self.book.buffer_added();
        match self.slots.iter().position(Option::is_none) {
            Some(free) => self.slots[free] = Some(key),
            None => self.slots.push(Some(key)),
        }
    }

    fn removed(&mut self, key: BufferKey) {
        self.removed = true;
        if self.book.buffer_removed(key) {
            tracing::debug!("screen-capture portal: a held buffer was removed; its lease is inert");
        }
        // PipeWire frees the buffer: never re-queue it.
        self.dequeued.retain(|_, buffer| buffer.key() != key);
        if self.parked.as_ref().is_some_and(|p| p.buffer.key() == key) {
            self.parked = None;
        }
        self.sync.forget(&key);
        for slot in &mut self.slots {
            if *slot == Some(key) {
                *slot = None;
            }
        }
        self.backings.remove(&key);
    }

    fn identity(&self, key: BufferKey) -> BufferIdentity {
        let slot = self
            .slots
            .iter()
            .position(|s| *s == Some(key))
            .and_then(|i| u32::try_from(i).ok())
            .unwrap_or(u32::MAX);
        BufferIdentity {
            pool_generation: self.generation,
            slot,
        }
    }

    /// The refusal of this episode (`new_episode` the first time).
    const fn exhausted(&mut self) -> Exhausted {
        let new_episode = !self.exhausted;
        if new_episode {
            self.exhausted = true;
            self.episodes = self.episodes.saturating_add(1);
        }
        Exhausted {
            new_episode,
            episode: self.episodes,
        }
    }

    /// The parked buffer `key` finished its acquire wait.
    fn acquired(&mut self, key: BufferKey, result: Result<(), String>) {
        if let Some(parked) = &mut self.parked
            && parked.buffer.key() == key
        {
            parked.acquired = Some(result);
        }
    }

    /// The next buffer to cut a frame from, its acquire point signalled:
    /// the parked one once its wait succeeded, else the newest ready one
    /// (older ones go back, counted in `skipped`). `None` while a wait
    /// runs or when nothing is ready; a buffer whose acquire point did not
    /// signal goes straight back.
    fn next_buffer(&mut self, stream: &Stream, skipped: &mut u32) -> Option<Buffer> {
        match self.parked.take() {
            Some(Parked {
                buffer,
                acquired: Some(Ok(())),
            }) => return Some(buffer),
            Some(Parked {
                buffer,
                acquired: Some(Err(why)),
            }) => {
                tracing::debug!(%why, "screen-capture portal: frame dropped");
                buffer.requeue(stream, &mut self.sync);
                *skipped = skipped.saturating_add(1);
            }
            Some(waiting) => {
                self.parked = Some(waiting);
                return None;
            }
            None => {}
        }
        let buffer = newest(stream, &mut self.sync, skipped)?;
        let Some(timeline) = buffer.timeline() else {
            return Some(buffer);
        };
        let polled = self
            .sync
            .poll(buffer.key(), timeline.fds, timeline.acquire_point);
        let wait = match polled {
            Ok(Acquire::Ready) => return Some(buffer),
            Ok(Acquire::Pending(wait)) => wait,
            Err(why) => {
                tracing::debug!(%why, "screen-capture portal: frame dropped");
                buffer.requeue(stream, &mut self.sync);
                *skipped = skipped.saturating_add(1);
                return None;
            }
        };
        if self.waiter.is_none() {
            match AcquireWaiter::spawn(self.stream, self.tx.clone()) {
                Ok(waiter) => self.waiter = Some(waiter),
                Err(err) => {
                    tracing::warn!(%err, "screen-capture portal: no acquire waiter thread");
                    buffer.requeue(stream, &mut self.sync);
                    *skipped = skipped.saturating_add(1);
                    return None;
                }
            }
        }
        let key = buffer.key();
        let queued = self
            .waiter
            .as_ref()
            .is_some_and(|waiter| waiter.jobs.send((key, wait)).is_ok());
        if queued {
            self.parked = Some(Parked {
                buffer,
                acquired: None,
            });
        } else {
            buffer.requeue(stream, &mut self.sync);
            *skipped = skipped.saturating_add(1);
        }
        None
    }

    /// Re-queue the buffer behind `release`, unless it is gone.
    fn release(&mut self, stream: &Stream, release: &Release) {
        self.catch_up();
        let Some(_key) = self.book.release(release.id) else {
            return;
        };
        let Some(buffer) = self.dequeued.remove(&release.id) else {
            return;
        };
        // Nothing reads the buffer any more: the producer may render.
        buffer.requeue(stream, &mut self.sync);
    }
}

enum Mode {
    Shot { kept: Vec<Buffer> },
    Live(Live),
}

/// One stream's state, on the loop thread.
struct StreamData {
    index: usize,
    source: Source,
    setup: Setup,
    info: VideoInfoRaw,
    negotiated: Option<Negotiated>,
    shared: Rc<RefCell<Shared>>,
    mode: Mode,
    /// Frames dropped since the last delivered one.
    skipped: u32,
}

/// One frame per part. Returns the frames in part order.
///
/// # Errors
/// PipeWire failures, no frame within [`SHOT_TIMEOUT`].
pub(super) fn screenshot(
    remote: Remote,
    parts: Vec<Part>,
    setup: Setup,
) -> Result<Vec<Captured>, CaptureError> {
    let (_thread, report) = LoopThread::spawn(
        ID,
        "istmo-screen-capture-portal",
        remote,
        SHOT_TIMEOUT,
        move |ctx, ready| run(ctx, ready, parts, &setup, None),
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
    setup: Setup,
    live: Live,
) -> Result<(LoopThread, Negotiated), CaptureError> {
    let (thread, report) = LoopThread::spawn(
        ID,
        "istmo-screen-capture-portal",
        remote,
        pwx::SETUP_TIMEOUT,
        move |ctx, ready| run(ctx, ready, vec![part], &setup, Some(live)),
    )?;
    match report {
        Report::Negotiated(negotiated) => Ok((thread, negotiated)),
        Report::Shots(_) => Err(error(
            "open",
            "the stream reported frames instead of a format",
        )),
    }
}

type Pair = (StreamRc, Rc<RefCell<StreamData>>, Rc<RefCell<Leases>>);

#[expect(
    clippy::too_many_lines,
    reason = "one setup sequence of the loop thread: streams, listeners, releases, then the loop"
)]
fn run(
    ctx: LoopCtx,
    ready: &flume::Sender<Result<Report, CaptureError>>,
    parts: Vec<Part>,
    setup: &Setup,
    live: Option<Live>,
) {
    let fail = |err| {
        // The caller gave up waiting when this fails.
        let _unwanted = ready.send(Err(err));
    };
    let enum_formats = match pods::enum_formats(
        &setup.tiers,
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
    let (release_tx, release_rx) = pw::channel::channel::<Wake>();
    let mut live = live;
    let mut streams: Vec<Pair> = Vec::new();
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
        let mode = live
            .take()
            .map_or_else(|| Mode::Shot { kept: Vec::new() }, Mode::Live);
        let data = Rc::new(RefCell::new(StreamData {
            index,
            source: part.source,
            setup: setup.clone(),
            info: VideoInfoRaw::default(),
            negotiated: None,
            shared: Rc::clone(&shared),
            mode,
            skipped: 0,
        }));
        let pending = PendingChanges::default();
        let leases = Rc::new(RefCell::new(Leases::new(
            release_tx.clone(),
            index,
            Rc::clone(&pending),
            ExplicitSync::new(setup.producer),
        )));
        let out = match &data.borrow().mode {
            Mode::Live(live) => Some(live.out.clone()),
            Mode::Shot { .. } => None,
        };
        // Every callback skips a re-entrant call (PipeWire calling back
        // from inside a call this module made while borrowing) instead of
        // panicking across the FFI boundary.
        let listener = stream
            .add_local_listener_with_user_data(Rc::clone(&data))
            .state_changed({
                let shared = Rc::clone(&shared);
                move |_, _, old, new| {
                    if let Some(end) = StreamEnd::of(&old, &new) {
                        on_end(out.as_ref(), &shared, end_error(end));
                    }
                }
            })
            .param_changed(|stream, data, id, param| {
                if let Ok(mut data) = data.try_borrow_mut() {
                    on_param(stream, &mut data, id, param);
                }
            })
            .add_buffer({
                let (leases, pending) = (Rc::clone(&leases), Rc::clone(&pending));
                move |_, _, buffer| {
                    if let Some(key) = BufferKey::of(buffer) {
                        Leases::report(&leases, &pending, PoolChange::Added(key));
                    }
                }
            })
            .remove_buffer({
                let (leases, pending) = (Rc::clone(&leases), Rc::clone(&pending));
                move |_, _, buffer| {
                    if let Some(key) = BufferKey::of(buffer) {
                        Leases::report(&leases, &pending, PoolChange::Removed(key));
                    }
                }
            })
            .process({
                let leases = Rc::clone(&leases);
                move |stream, data| {
                    if let (Ok(mut data), Ok(mut leases)) =
                        (data.try_borrow_mut(), leases.try_borrow_mut())
                    {
                        leases.catch_up();
                        on_process(stream, &mut data, &mut leases);
                    }
                }
            })
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
        streams.push((stream, data, leases));
    }
    // A frame dropped its last clone: re-queue its buffer, then let a
    // stalled stream pull again. Attached after the streams, so it is
    // detached before them and never re-queues into a destroyed stream.
    let mainloop = ctx.mainloop.clone();
    let _releases = release_rx.attach(mainloop.loop_(), {
        let streams: Vec<Pair> = streams
            .iter()
            .map(|(s, d, l)| (s.clone(), Rc::clone(d), Rc::clone(l)))
            .collect();
        let retry = release_tx.clone();
        move |wake: Wake| {
            let index = match &wake {
                Wake::Release(release) => release.stream,
                Wake::Acquired { stream, .. } => *stream,
            };
            let Some((stream, data, leases)) = streams.get(index) else {
                return;
            };
            match (data.try_borrow_mut(), leases.try_borrow_mut()) {
                (Ok(mut data), Ok(mut leases)) => {
                    match wake {
                        Wake::Release(release) => leases.release(stream, &release),
                        Wake::Acquired { key, result, .. } => {
                            leases.catch_up();
                            leases.acquired(key, result);
                        }
                    }
                    on_process(stream, &mut data, &mut leases);
                }
                // Dispatched from inside a callback that holds the
                // stream's state: posted again for the next loop
                // iteration, so neither a hold nor a parked buffer leaks.
                _ => {
                    let _sent = retry.send(wake);
                }
            }
        }
    });
    drop(release_tx);
    ctx.run();
    for (_, data, leases) in &streams {
        let mut data = data.borrow_mut();
        if let Mode::Live(live) = &data.mode {
            live.out.stop(crate::types::StopReason::Closed);
        }
        // Buffers still held by frames (or parked) go back with the
        // stream itself.
        let mut leases = leases.borrow_mut();
        leases.dequeued.clear();
        leases.parked = None;
        if let Mode::Shot { kept } = &mut data.mode {
            kept.clear();
        }
    }
    // Listeners first: tearing the streams down calls back no more.
    drop(listeners);
    drop(streams);
}

/// PipeWire's "no more input formats": the producer offers none of the
/// formats (and memory) the stream listed.
const NO_FORMAT: &str = "no more input formats";

/// The session error of a stream end. A producer that offers none of the
/// listed formats is a format the backend cannot serve, not a protocol
/// failure.
fn end_error(end: StreamEnd) -> CaptureError {
    match end {
        StreamEnd::Error(message) if message.contains(NO_FORMAT) => {
            CaptureError::unsatisfiable(ID, crate::types::Requirement::Formats)
        }
        other => other.into_error(ID),
    }
}

/// End a stream: fail the session's output, answer a waiting caller,
/// stop the loop. Takes no stream data: PipeWire may report a state
/// change from inside a call made while that data is borrowed.
fn on_end(out: Option<&FrameOut>, shared: &RefCell<Shared>, err: CaptureError) {
    if let Some(out) = out {
        out.fail(err.clone());
    }
    if let Ok(mut shared) = shared.try_borrow_mut() {
        shared.report(Err(err));
        shared.mainloop.quit();
    }
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
    let flags = data.info.flags().bits();
    if flags & VIDEO_FLAG_MODIFIER_FIXATION_REQUIRED != 0 {
        // The producer still has to pick a modifier; the fixated format
        // follows.
        return;
    }
    let Some(format) = pods::pixel_format(data.info.format()) else {
        tracing::warn!(format = ?data.info.format(), "screen-capture portal: unexpected video format");
        return;
    };
    let modifier = (flags & VIDEO_FLAG_MODIFIER != 0).then(|| data.info.modifier());
    let Some(tier) = pods::tier_of(&data.setup.tiers, format, modifier) else {
        tracing::warn!(
            ?format,
            ?modifier,
            "screen-capture portal: format outside every tier"
        );
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
        modifier,
    };
    let previous = data.negotiated.replace(negotiated);
    if let Mode::Live(live) = &data.mode {
        if previous.is_some_and(|old| old.size != negotiated.size) {
            live.out.event(CaptureEvent::SourceResized(negotiated.size));
        }
        data.shared
            .borrow_mut()
            .report(Ok(Report::Negotiated(negotiated)));
    }
    let depth = match &data.mode {
        Mode::Live(live) => live.pool_depth,
        Mode::Shot { .. } => 2,
    };
    let dmabuf = modifier.is_some();
    let params = pods::buffers(depth, dmabuf).and_then(|buffers| {
        pods::metas(data.setup.cursor_meta, dmabuf).map(|metas| (buffers, metas))
    });
    let sent = params.and_then(|(buffers, metas)| {
        let mut pods = Vec::new();
        for bytes in buffers.iter().chain(&metas) {
            pods.push(pwx::pod(ID, bytes)?);
        }
        stream
            .update_params(&mut pods)
            .map_err(|err| error("update_params(Buffers, Meta)", err))
    });
    if let Err(err) = sent {
        let out = match &data.mode {
            Mode::Live(live) => Some(live.out.clone()),
            Mode::Shot { .. } => None,
        };
        on_end(out.as_ref(), &data.shared, err);
    }
}

/// Re-queue every ready buffer; how many there were.
fn drain(stream: &Stream, sync: &mut ExplicitSync<BufferKey>) -> u32 {
    let mut dropped = 0;
    while let Some(buffer) = Buffer::dequeue(stream) {
        buffer.requeue(stream, sync);
        dropped += 1;
    }
    dropped
}

/// The newest ready buffer; older ones go straight back (`skipped`).
fn newest(
    stream: &Stream,
    sync: &mut ExplicitSync<BufferKey>,
    skipped: &mut u32,
) -> Option<Buffer> {
    let mut newest: Option<Buffer> = None;
    while let Some(buffer) = Buffer::dequeue(stream) {
        if let Some(older) = newest.replace(buffer) {
            older.requeue(stream, sync);
            *skipped = skipped.saturating_add(1);
        }
    }
    newest
}

/// A frame cut from a dequeued buffer, before it is leased or kept.
struct Cut {
    meta: FrameMeta,
    backing: FrameBacking,
}

enum FrameBacking {
    DmaBuf(Arc<DmaBuf>),
    Mapped(MappedFrame),
    /// Copied out (`ExhaustionReadback`): needs no lease.
    Copied(Vec<u8>),
}

/// A header pts within a minute of `CLOCK_MONOTONIC` now is on that
/// clock; anything else is a producer-specific clock and is replaced by
/// the arrival time.
fn plausible_monotonic(pts: u64) -> bool {
    const WINDOW_NS: u64 = 60_000_000_000;
    monotonic_ns().abs_diff(pts) < WINDOW_NS
}

fn orientation(transform: Option<u32>) -> Orientation {
    use wayland_client::protocol::wl_output::Transform;
    transform
        .and_then(|t| Transform::try_from(t).ok())
        .map_or(Orientation::UPRIGHT, |t| {
            super::super::wayland::orientation_of(t, false)
        })
}

/// The shared backing of `buffer`: its DMA-BUF fds duplicated, or its
/// `MemFd` mapped, once per buffer.
fn backing(
    leases: &mut Leases,
    buffer: &Buffer,
    negotiated: Negotiated,
) -> Result<Backing, String> {
    if let Some(backing) = leases.backings.get(&buffer.key()) {
        return Ok(backing.clone());
    }
    let planes = buffer.planes();
    let first = planes.first().ok_or("a buffer without planes")?;
    let backing = if let Some(modifier) = negotiated.modifier {
        let planes = planes
            .iter()
            .map(|plane| {
                let fd = plane.fd.ok_or("a DMA-BUF plane without an fd")?;
                // The frame owns its duplicate: PipeWire closes its own
                // on renegotiation while the frame may still live.
                let fd = rustix::io::fcntl_dupfd_cloexec(fd, 0)
                    .map_err(|e| format!("dup a DMA-BUF fd: {e}"))?;
                let stride = u32::try_from(plane.chunk_stride)
                    .ok()
                    .filter(|s| *s > 0)
                    .ok_or("a DMA-BUF plane without a stride")?;
                Ok(DmaBufPlane {
                    fd,
                    offset: plane.chunk_offset,
                    stride,
                    size: (plane.maxsize > 0).then_some(u64::from(plane.maxsize)),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Backing::DmaBuf(Arc::new(DmaBuf {
            width: negotiated.size.width,
            height: negotiated.size.height,
            fourcc: negotiated.format.drm_fourcc(),
            modifier,
            planes,
        }))
    } else {
        if first.kind != raw::DATA_MEM_FD {
            return Err(format!("unexpected shared memory type {}", first.kind));
        }
        let fd = first.fd.ok_or("a MemFd plane without an fd")?;
        let mapping = Mapping::new(
            fd,
            u64::from(first.mapoffset),
            usize::try_from(first.maxsize).unwrap_or(0),
        )
        .map_err(|e| e.to_string())?;
        Backing::Mapped(Arc::new(mapping))
    };
    leases.backings.insert(buffer.key(), backing.clone());
    Ok(backing)
}

#[expect(
    clippy::too_many_lines,
    reason = "sync, metas and backing of one frame, read in one pass over the dequeued buffer"
)]
/// Cut a frame out of `buffer` (dequeued, its acquire point signalled):
/// read its metas, share its backing. `copy` reads shared memory out
/// instead (a lease-free `CopyToCpu` frame).
fn cut(
    data: &StreamData,
    leases: &mut Leases,
    buffer: &Buffer,
    negotiated: Negotiated,
    copy: bool,
) -> Result<Option<Cut>, String> {
    let planes = buffer.planes();
    let Some(first) = planes.first().copied() else {
        return Err("a buffer without planes".to_owned());
    };
    if first.chunk_size == 0 && negotiated.modifier.is_none() {
        // The producer has not rendered into it yet.
        return Ok(None);
    }
    // A timeline buffer comes here once its acquire point signalled.
    let sync = if buffer.timeline().is_none() && negotiated.modifier.is_some() {
        SyncKind::Implicit
    } else {
        SyncKind::Ready
    };
    let row_bytes = negotiated
        .size
        .width
        .saturating_mul(negotiated.format.bytes_per_pixel());
    let stride = u32::try_from(first.chunk_stride)
        .ok()
        .filter(|s| *s > 0)
        .unwrap_or(row_bytes);
    let (backing, planes, copied) = match backing(leases, buffer, negotiated)? {
        Backing::DmaBuf(dmabuf) => {
            let planes = dmabuf
                .planes
                .iter()
                .map(|p| PlaneLayout {
                    offset: p.offset,
                    stride: p.stride,
                    size: p
                        .size
                        .unwrap_or_else(|| u64::from(p.stride) * u64::from(negotiated.size.height)),
                })
                .collect();
            (FrameBacking::DmaBuf(dmabuf), planes, None)
        }
        Backing::Mapped(mapping) => {
            let layout = RowLayout {
                stride,
                row_bytes,
                height: negotiated.size.height,
            };
            let range = layout
                .frame_range(mapping.bytes().len(), first.chunk_offset)
                .ok_or("the shared memory is shorter than the frame")?;
            let planes = vec![PlaneLayout {
                offset: 0,
                stride,
                size: u64::try_from(range.len()).unwrap_or(u64::MAX),
            }];
            if copy {
                let bytes = mapping.bytes().get(range).unwrap_or_default().to_vec();
                (
                    FrameBacking::Copied(bytes),
                    planes,
                    Some(CopyStage::ExhaustionReadback),
                )
            } else {
                let frame =
                    MappedFrame::new(mapping, range).ok_or("the frame lies outside the mapping")?;
                (FrameBacking::Mapped(frame), planes, None)
            }
        }
    };
    let bounds = Rect::from_size(negotiated.size);
    let content = buffer
        .crop()
        .and_then(|crop| crop.intersect(bounds))
        .unwrap_or(bounds);
    let damage = buffer.damage().map_or(Damage::Full, |rects| {
        Damage::clipped(rects, negotiated.size)
    });
    let pts_ns = buffer
        .pts()
        .filter(|pts| plausible_monotonic(*pts))
        .unwrap_or_else(monotonic_ns);
    let cursor: Option<CursorState> = data.setup.cursor_meta.then(|| buffer.cursor()).flatten();
    let meta = FrameMeta {
        seq: 0,
        pts_ns,
        pts_clock: PtsClock::Monotonic,
        wall_us: 0,
        size: negotiated.size,
        content_rect: content,
        format: negotiated.format,
        memory: if matches!(backing, FrameBacking::DmaBuf(_)) {
            BufferMemory::DmaBuf
        } else {
            BufferMemory::Cpu
        },
        planes,
        modifier: negotiated.modifier,
        buffer: BufferIdentity::default(),
        tier: negotiated.tier,
        copied,
        producer: negotiated
            .modifier
            .and(data.setup.producer)
            .map(GpuIdentity::Drm),
        colour: Colour::SRGB,
        hdr: None,
        orientation: orientation(buffer.transform()),
        damage,
        cursor,
        sync,
        scale_permille: data.source.scale(),
        source_id: data.source.id.clone(),
        repeat: false,
        skipped_before: data.skipped,
        protected_content: false,
        composite: None,
    };
    Ok(Some(Cut { meta, backing }))
}

fn on_process(stream: &Stream, data: &mut StreamData, leases: &mut Leases) {
    let Some(negotiated) = data.negotiated else {
        drain(stream, &mut leases.sync);
        return;
    };
    if matches!(data.mode, Mode::Shot { .. }) {
        shot(stream, data, leases, negotiated);
    } else {
        deliver(stream, data, leases, negotiated);
    }
}

fn shot(stream: &Stream, data: &mut StreamData, leases: &mut Leases, negotiated: Negotiated) {
    let done = data
        .shared
        .borrow()
        .shots
        .get(data.index)
        .is_none_or(Option::is_some);
    if done {
        drain(stream, &mut leases.sync);
        return;
    }
    let mut skipped = 0;
    let Some(buffer) = leases.next_buffer(stream, &mut skipped) else {
        return;
    };
    let cut = match cut(data, leases, &buffer, negotiated, false) {
        Ok(Some(cut)) => cut,
        Ok(None) => {
            buffer.requeue(stream, &mut leases.sync);
            return;
        }
        Err(why) => {
            tracing::warn!(%why, "screen-capture portal: frame dropped");
            buffer.requeue(stream, &mut leases.sync);
            return;
        }
    };
    // Kept dequeued until the stream ends: the producer never renders
    // into it again, so the frame stays what it shows.
    if let Mode::Shot { kept } = &mut data.mode {
        kept.push(buffer);
    }
    let buffer = match cut.backing {
        FrameBacking::DmaBuf(dmabuf) => NativeBuffer::from_dmabuf(dmabuf),
        FrameBacking::Mapped(frame) => NativeBuffer::from_bytes(frame),
        FrameBacking::Copied(bytes) => NativeBuffer::from_bytes(bytes),
    };
    let mut source = data.source.clone();
    source.physical_size = Some(negotiated.size);
    if let Some(geometry) = source.geometry.filter(|g| g.width > 0) {
        source.scale_permille = Some(negotiated.size.width.saturating_mul(1000) / geometry.width);
    }
    let captured = Captured {
        meta: FrameMeta {
            scale_permille: source.scale(),
            ..cut.meta
        },
        buffer,
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

fn deliver(stream: &Stream, data: &mut StreamData, leases: &mut Leases, negotiated: Negotiated) {
    let Mode::Live(live) = &data.mode else {
        return;
    };
    let (out, exhaustion, pool_depth) = (live.out.clone(), live.exhaustion, live.pool_depth);
    if out.is_stopped() {
        drain(stream, &mut leases.sync);
        data.shared.borrow().mainloop.quit();
        return;
    }
    if !out.wants_frames() {
        data.skipped = data.skipped.saturating_add(drain(stream, &mut leases.sync));
        return;
    }
    let full = !leases.book.has_room();
    // Stall: leave the buffers queued (the producer runs out and stops)
    // until a frame drops and its release pulls again.
    if full && exhaustion == Exhaustion::Stall {
        let exhausted = leases.exhausted();
        out.exhausted(exhausted, pool_depth, exhaustion);
        return;
    }
    let copy = full && exhaustion == Exhaustion::CopyToCpu && negotiated.modifier.is_none();
    let mut skipped = data.skipped;
    if full && !copy {
        // DropNewest (or CopyToCpu on DMA-BUF, refused at open): nothing
        // is read, so nothing waits for an acquire point.
        let dropped = newest(stream, &mut leases.sync, &mut skipped);
        data.skipped = skipped;
        let Some(buffer) = dropped else {
            return;
        };
        let exhausted = leases.exhausted();
        out.exhausted(exhausted, pool_depth, exhaustion);
        buffer.requeue(stream, &mut leases.sync);
        data.skipped = data.skipped.saturating_add(1);
        return;
    }
    let next = leases.next_buffer(stream, &mut skipped);
    data.skipped = skipped;
    let Some(buffer) = next else {
        return;
    };
    if copy {
        let exhausted = leases.exhausted();
        out.exhausted(exhausted, pool_depth, exhaustion);
    } else {
        leases.exhausted = false;
    }
    let cut = match cut(data, leases, &buffer, negotiated, copy) {
        Ok(Some(cut)) => cut,
        Ok(None) => {
            buffer.requeue(stream, &mut leases.sync);
            return;
        }
        Err(why) => {
            tracing::debug!(%why, "screen-capture portal: frame dropped");
            buffer.requeue(stream, &mut leases.sync);
            data.skipped = data.skipped.saturating_add(1);
            return;
        }
    };
    let Cut { mut meta, backing } = cut;
    let buffer = match backing {
        FrameBacking::Copied(bytes) => {
            // Read out: the buffer goes back at once.
            buffer.requeue(stream, &mut leases.sync);
            NativeBuffer::from_bytes(bytes)
        }
        FrameBacking::DmaBuf(_) | FrameBacking::Mapped(_) => {
            let key = buffer.key();
            let Some(id) = leases.book.hold(key) else {
                buffer.requeue(stream, &mut leases.sync);
                return;
            };
            let identity = leases.identity(key);
            meta.buffer = identity;
            leases.dequeued.insert(id, buffer);
            let (tx, stream_index) = (leases.tx.clone(), leases.stream);
            let native = match backing {
                FrameBacking::DmaBuf(dmabuf) => NativeBuffer::from_dmabuf(dmabuf),
                FrameBacking::Mapped(frame) => NativeBuffer::from_bytes(frame),
                FrameBacking::Copied(bytes) => NativeBuffer::from_bytes(bytes),
            };
            native.with_identity(identity.into()).with_release(move || {
                // Fails only once the loop ended (and the buffers with it).
                let _sent = tx.send(Wake::Release(Release {
                    stream: stream_index,
                    id,
                }));
            })
        }
    };
    match out.deliver(meta, buffer) {
        Delivery::Delivered => data.skipped = 0,
        Delivery::Closed => data.shared.borrow().mainloop.quit(),
        Delivery::Deferred | Delivery::Dropped => {}
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::{
        BufferKey, ExplicitSync, HeldBuffers, Leases, PendingChanges, PoolChange, RowLayout,
    };

    fn key(addr: usize) -> BufferKey {
        BufferKey::of(std::ptr::without_provenance_mut(addr)).expect("non-null")
    }

    fn leases() -> (RefCell<Leases>, PendingChanges) {
        let (tx, _rx) = pipewire::channel::channel();
        let pending = PendingChanges::default();
        let leases = Leases::new(tx, 0, pending.clone(), ExplicitSync::new(None));
        (RefCell::new(leases), pending)
    }

    #[test]
    fn pool_changes_reported_while_borrowed_are_applied_in_order() {
        let (leases, pending) = leases();
        Leases::report(&leases, &pending, PoolChange::Added(key(0x10)));
        {
            let _busy = leases.borrow_mut();
            Leases::report(&leases, &pending, PoolChange::Added(key(0x20)));
            Leases::report(&leases, &pending, PoolChange::Removed(key(0x10)));
            // A new buffer at the freed address.
            Leases::report(&leases, &pending, PoolChange::Added(key(0x10)));
        }
        assert_eq!(pending.borrow().len(), 3, "queued, not dropped");
        // The next report applies the queue first.
        Leases::report(&leases, &pending, PoolChange::Added(key(0x30)));
        let leases = leases.borrow();
        assert!(pending.borrow().is_empty());
        assert_eq!(leases.book.limit(), 2, "three live buffers");
        for addr in [0x10, 0x20, 0x30] {
            assert_ne!(
                leases.identity(key(addr)).slot,
                u32::MAX,
                "{addr:#x} has a slot"
            );
        }
        assert_eq!(
            leases.generation,
            crate::types::BufferIdentity::default().pool_generation + 2,
            "the removal started a new generation"
        );
    }

    #[test]
    fn catch_up_applies_a_removal_queued_behind_an_addition() {
        let (leases, pending) = leases();
        pending
            .borrow_mut()
            .extend([PoolChange::Added(key(0x10)), PoolChange::Removed(key(0x10))]);
        let mut leases = leases.borrow_mut();
        leases.catch_up();
        assert_eq!(leases.book.limit(), 0);
        assert_eq!(leases.identity(key(0x10)).slot, u32::MAX);
    }

    const LAYOUT: RowLayout = RowLayout {
        stride: 64,
        row_bytes: 40,
        height: 3,
    };

    fn pool(n: usize) -> HeldBuffers<u32> {
        let mut book = HeldBuffers::default();
        (0..n).for_each(|_| book.buffer_added());
        book
    }

    #[test]
    fn holds_all_but_one_buffer() {
        let mut book = pool(4);
        assert_eq!(book.limit(), 3);
        let ids: Vec<_> = (0..3).map(|b| book.hold(b).expect("under limit")).collect();
        assert_eq!(book.hold(3), None, "the producer keeps one buffer");
        assert_eq!(book.release(ids[1]), Some(1));
        assert!(book.hold(3).is_some());
    }

    #[test]
    fn tiny_or_unknown_pool_holds_nothing() {
        assert_eq!(pool(0).hold(0), None);
        assert_eq!(pool(1).hold(0), None);
    }

    #[test]
    fn release_returns_each_hold_once() {
        let mut book = pool(3);
        let id = book.hold(7).expect("under limit");
        assert_eq!(book.release(id), Some(7));
        assert_eq!(book.release(id), None);
        let again = book.hold(7).expect("under limit");
        assert_ne!(again, id);
        assert_eq!(book.release(id), None);
        assert_eq!(book.release(again), Some(7));
    }

    #[test]
    fn removed_buffers_make_late_releases_inert() {
        let mut book = pool(3);
        let held = book.hold(1).expect("under limit");
        let kept = book.hold(2).expect("under limit");
        assert!(book.buffer_removed(1));
        assert!(book.buffer_removed(2));
        assert!(!book.buffer_removed(3));
        assert_eq!(book.limit(), 0);
        (0..3).for_each(|_| book.buffer_added());
        let fresh = book.hold(1).expect("under limit");
        assert_eq!(book.release(held), None);
        assert_eq!(book.release(kept), None);
        assert_eq!(book.release(fresh), Some(1));
    }

    #[test]
    fn shrinking_pool_lowers_the_limit_below_current_holds() {
        let mut book = pool(4);
        let a = book.hold(0).expect("under limit");
        book.hold(1).expect("under limit");
        assert!(!book.buffer_removed(3));
        assert!(!book.buffer_removed(2));
        assert_eq!(book.limit(), 1);
        assert_eq!(book.hold(2), None);
        assert_eq!(book.release(a), Some(0));
        assert_eq!(book.hold(2), None, "still at the limit with one held");
    }

    #[test]
    fn a_producer_without_listed_formats_is_a_format_miss() {
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

    #[test]
    fn video_transforms_become_orientations() {
        use crate::types::Rotation;
        assert!(super::orientation(None).is_upright());
        assert_eq!(super::orientation(Some(1)).rotation, Rotation::R90);
        let flipped = super::orientation(Some(7));
        assert!(flipped.mirrored);
        assert_eq!(flipped.rotation, Rotation::R270);
    }
}
