//! Where backends deliver: frame pacing, pause, in-meta crops, sequence
//! numbers, per-frame wall clock, and session events.

#![allow(
    dead_code,
    reason = "the producer side is used by the platform backends; a build without one for its target uses less of it"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "the state lock is held on purpose across a frame hand-off; where it fires depends on the build"
)]

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use istmo_core::{FrameSender, NativeBuffer};

use crate::composite::logical_to_buffer;
use crate::pixels::OrientationMap;
use crate::pool::Exhausted;
use crate::types::{
    AudioMeta, BackendId, CaptureError, CaptureEvent, Damage, Exhaustion, ExhaustionReport,
    FrameMeta, FrameRate, NegotiatedStream, Rect, StopReason,
};

use super::clock;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Short state updates only; a poisoned lock still holds valid state.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A frame arriving within `period / SLACK` of its due time counts as due.
const SLACK: u32 = 20;

/// Events kept for replay to every new subscriber (the newest ones).
const BACKLOG: usize = 64;
/// Events queued per subscriber before new ones are dropped.
const SUBSCRIBER_DEPTH: usize = 256;

type EventItem = Result<CaptureEvent, CaptureError>;

#[derive(Debug, Default)]
struct HubState {
    /// The stream as last announced by an event that already left the
    /// backlog, replayed as `Started` so a late subscriber still learns
    /// what the session negotiated.
    baseline: Option<NegotiatedStream>,
    backlog: VecDeque<CaptureEvent>,
    subscribers: Vec<flume::Sender<EventItem>>,
    ended: bool,
}

/// A session's event fan-out. Every subscriber first receives the
/// session's state so far (the newest [`BACKLOG`] events, preceded by
/// `Started` with the last stream announced before them), then live
/// events.
///
/// Restore tokens never travel in events: they are returned once, by
/// the `open` call that produced them.
#[derive(Debug, Default)]
pub(crate) struct EventHub {
    state: Mutex<HubState>,
}

impl EventHub {
    pub(crate) fn emit(&self, mut event: CaptureEvent) {
        if let CaptureEvent::Started(stream) | CaptureEvent::Renegotiated(stream) = &mut event {
            stream.restore_token = None;
        }
        let mut state = lock(&self.state);
        if state.ended {
            return;
        }
        if state.backlog.len() == BACKLOG
            && let Some(CaptureEvent::Started(stream) | CaptureEvent::Renegotiated(stream)) =
                state.backlog.pop_front()
        {
            state.baseline = Some(stream);
        }
        state.backlog.push_back(event.clone());
        state
            .subscribers
            .retain(|tx| match tx.try_send(Ok(event.clone())) {
                Ok(()) => true,
                Err(flume::TrySendError::Full(_)) => {
                    tracing::warn!("screen-capture events subscriber is full; event dropped");
                    true
                }
                Err(flume::TrySendError::Disconnected(_)) => false,
            });
    }

    /// A new subscriber, starting with the session's state so far. After
    /// the session ended the stream completes once that is read.
    pub(crate) fn subscribe(&self) -> flume::Receiver<EventItem> {
        let (tx, rx) = flume::bounded(SUBSCRIBER_DEPTH);
        let mut state = lock(&self.state);
        if let Some(stream) = &state.baseline {
            let _queued = tx.try_send(Ok(CaptureEvent::Started(stream.clone())));
        }
        for event in &state.backlog {
            let _queued = tx.try_send(Ok(event.clone()));
        }
        if !state.ended {
            state.subscribers.push(tx);
        }
        rx
    }

    /// No more events: every subscriber's stream completes.
    pub(crate) fn end(&self) {
        let mut state = lock(&self.state);
        state.ended = true;
        state.subscribers.clear();
    }
}

/// What happened to a delivered frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Delivery {
    /// In the sink.
    Delivered,
    /// Held by the pacer; it will be delivered (or merged into a newer
    /// frame) at the end of the period.
    Deferred,
    /// Dropped: the session is paused, or `OnDamage` saw no change.
    Dropped,
    /// The sink or the session is gone: stop capturing.
    Closed,
}

/// The kept crop of a region session, logical and source-relative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Crop {
    pub(crate) logical: Rect,
    pub(crate) scale_permille: u32,
}

#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent switches of one session"
)]
struct OutState {
    seq: u64,
    fps: FrameRate,
    paused: bool,
    stopped: bool,
    crop: Option<Crop>,
    /// `OrientationHandling::Upright`: CPU frames are turned here.
    upright: bool,
    pending: Option<(FrameMeta, NativeBuffer)>,
    /// Frames merged into `pending` (they count as skipped).
    merged: u32,
    /// The last delivered frame, kept for `Exact` repeats.
    last: Option<(FrameMeta, NativeBuffer)>,
    last_push: Option<Instant>,
    pacer: bool,
}

#[derive(Debug)]
struct OutInner {
    backend: BackendId,
    sender: FrameSender<FrameMeta>,
    events: Arc<EventHub>,
    state: Mutex<OutState>,
    wake: Condvar,
}

/// A session's video output. Cheap to clone; backends keep one on their
/// capture thread.
#[derive(Debug, Clone)]
pub(crate) struct FrameOut {
    inner: Arc<OutInner>,
}

fn period(fps: FrameRate) -> Option<Duration> {
    match fps {
        FrameRate::Native => None,
        FrameRate::Max(n) | FrameRate::Exact(n) | FrameRate::OnDamage(n) => {
            Some(Duration::from_secs(1) / n.max(1))
        }
    }
}

impl FrameOut {
    pub(crate) fn new(
        backend: BackendId,
        sender: FrameSender<FrameMeta>,
        events: Arc<EventHub>,
        fps: FrameRate,
    ) -> Self {
        let out = Self {
            inner: Arc::new(OutInner {
                backend,
                sender,
                events,
                state: Mutex::new(OutState {
                    seq: 0,
                    fps,
                    paused: false,
                    stopped: false,
                    crop: None,
                    upright: false,
                    pending: None,
                    merged: 0,
                    last: None,
                    last_push: None,
                    pacer: false,
                }),
                wake: Condvar::new(),
            }),
        };
        if matches!(fps, FrameRate::Exact(_)) {
            let mut state = lock(&out.inner.state);
            out.ensure_pacer(&mut state);
        }
        out
    }

    pub(crate) fn backend(&self) -> BackendId {
        self.inner.backend
    }

    pub(crate) fn event(&self, event: CaptureEvent) {
        self.inner.events.emit(event);
    }

    /// Whether the session still wants frames (not stopped, not paused,
    /// sink open). Producers that can stop pulling check it.
    pub(crate) fn wants_frames(&self) -> bool {
        let state = lock(&self.inner.state);
        !state.stopped && !state.paused && !self.inner.sender.is_closed()
    }

    /// Whether the session ended (sink gone or stopped).
    pub(crate) fn is_stopped(&self) -> bool {
        lock(&self.inner.state).stopped || self.inner.sender.is_closed()
    }

    pub(crate) fn fps(&self) -> FrameRate {
        lock(&self.inner.state).fps
    }

    pub(crate) fn set_fps(&self, fps: FrameRate) {
        let mut state = lock(&self.inner.state);
        state.fps = fps;
        if !matches!(fps, FrameRate::Exact(_)) {
            state.last = None;
        }
        if matches!(fps, FrameRate::Exact(_)) {
            self.ensure_pacer(&mut state);
        }
        self.inner.wake.notify_all();
    }

    /// Block while the session is paused (and not stopped), at most
    /// `timeout`: a paused producer sleeps instead of polling.
    pub(crate) fn wait_while_paused(&self, timeout: Duration) {
        let state = lock(&self.inner.state);
        let _woken = self
            .inner
            .wake
            .wait_timeout_while(state, timeout, |s| s.paused && !s.stopped)
            .unwrap_or_else(PoisonError::into_inner);
    }

    pub(crate) fn set_paused(&self, paused: bool) {
        let mut state = lock(&self.inner.state);
        state.paused = paused;
        if paused {
            state.pending = None;
            state.last = None;
        }
        self.inner.wake.notify_all();
    }

    pub(crate) fn set_crop(&self, crop: Option<Crop>) {
        lock(&self.inner.state).crop = crop;
    }

    pub(crate) fn set_upright(&self, upright: bool) {
        lock(&self.inner.state).upright = upright;
    }

    /// Report a refused pool hold; an episode's first refusal emits
    /// `Exhausted` once.
    pub(crate) fn exhausted(&self, exhausted: Exhausted, pool_depth: u8, policy: Exhaustion) {
        if exhausted.new_episode {
            self.event(CaptureEvent::Exhausted(ExhaustionReport {
                episode: exhausted.episode,
                pool_depth,
                policy,
            }));
        }
    }

    /// Whether a frame may enter now: `Ok(upright)`, or why not.
    fn admits(&self) -> Result<bool, Delivery> {
        let mut state = lock(&self.inner.state);
        if state.stopped || self.inner.sender.is_closed() {
            state.stopped = true;
            return Err(Delivery::Closed);
        }
        if state.paused {
            return Err(Delivery::Dropped);
        }
        Ok(state.upright)
    }

    /// Hand a finished frame over. `meta.seq`, `meta.wall_us` and the
    /// in-meta crop are filled in here.
    pub(crate) fn deliver(&self, meta: FrameMeta, buffer: NativeBuffer) -> Delivery {
        let upright = match self.admits() {
            Ok(upright) => upright,
            Err(delivery) => return delivery,
        };
        // Turned outside the lock: a CPU rotation takes milliseconds and
        // must not stall the pacer or a concurrent update.
        let (mut meta, buffer) = if upright {
            match turn_upright(self.inner.backend, meta, buffer) {
                Ok(turned) => turned,
                Err(err) => {
                    self.fail(err);
                    return Delivery::Closed;
                }
            }
        } else {
            (meta, buffer)
        };
        let mut state = lock(&self.inner.state);
        if state.stopped {
            return Delivery::Closed;
        }
        if let Some(crop) = state.crop {
            meta.content_rect = crop_rect(&meta, crop);
        }
        match state.fps {
            FrameRate::Native => self.push(&mut state, meta, buffer),
            FrameRate::OnDamage(_) if !meta.damage.changed() => Delivery::Dropped,
            FrameRate::Max(_) | FrameRate::OnDamage(_) | FrameRate::Exact(_) => {
                // A frame due now goes out now (no added latency); one that
                // comes early waits for the end of the period. A producer
                // that already paces at the rate lands a hair early or late:
                // the slack keeps those on time instead of a period late.
                let due = state.pending.is_none()
                    && match (state.last_push, period(state.fps)) {
                        (Some(at), Some(period)) => at.elapsed() + period / SLACK >= period,
                        _ => true,
                    };
                if due {
                    let delivery = self.push(&mut state, meta, buffer);
                    if matches!(state.fps, FrameRate::Exact(_)) {
                        // Exact repeats an idle source from the pacer.
                        self.ensure_pacer(&mut state);
                    }
                    delivery
                } else {
                    Self::defer(&mut state, meta, buffer);
                    self.ensure_pacer(&mut state);
                    Delivery::Deferred
                }
            }
        }
    }

    fn defer(state: &mut OutState, mut meta: FrameMeta, buffer: NativeBuffer) {
        if let Some((older, _)) = state.pending.take() {
            state.merged = state
                .merged
                .saturating_add(1)
                .saturating_add(older.skipped_before);
            meta.damage = older.damage.merge(meta.damage);
        }
        state.pending = Some((meta, buffer));
    }

    fn push(&self, state: &mut OutState, mut meta: FrameMeta, buffer: NativeBuffer) -> Delivery {
        meta.seq = state.seq;
        state.seq += 1;
        meta.skipped_before = meta
            .skipped_before
            .saturating_add(std::mem::take(&mut state.merged));
        meta.wall_us = clock::wall_us_for(meta.pts_ns, meta.pts_clock);
        let keep = matches!(state.fps, FrameRate::Exact(_)).then(|| (meta.clone(), buffer.clone()));
        state.last_push = Some(Instant::now());
        match self.inner.sender.push(meta, buffer) {
            Ok(_) => {
                state.last = keep;
                Delivery::Delivered
            }
            Err(err) if err.is_closed() => {
                state.stopped = true;
                state.last = None;
                Delivery::Closed
            }
            // A full `DropNewest` sink refused it (the sink counts it).
            Err(_) => Delivery::Dropped,
        }
    }

    fn ensure_pacer(&self, state: &mut OutState) {
        if state.pacer || state.stopped {
            return;
        }
        let out = self.clone();
        let spawned = std::thread::Builder::new()
            .name("istmo-screen-capture-pacer".to_owned())
            .spawn(move || out.pace());
        match spawned {
            Ok(_) => state.pacer = true,
            Err(err) => tracing::warn!(%err, "screen-capture: cannot start the frame pacer"),
        }
    }

    /// The pacer thread: delivers deferred frames at the end of their
    /// period, and repeats the last frame for `Exact`.
    fn pace(&self) {
        let mut state = lock(&self.inner.state);
        loop {
            if state.stopped || self.inner.sender.is_closed() {
                state.pending = None;
                state.last = None;
                state.pacer = false;
                return;
            }
            let Some(period) = period(state.fps) else {
                // Back to Native: flush what is pending and stop pacing.
                if let Some((meta, buffer)) = state.pending.take() {
                    self.push(&mut state, meta, buffer);
                }
                state.pacer = false;
                return;
            };
            let due = state
                .last_push
                .map_or(Duration::ZERO, |at| period.saturating_sub(at.elapsed()));
            if !due.is_zero() {
                state = self
                    .inner
                    .wake
                    .wait_timeout(state, due)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
                continue;
            }
            if state.paused {
                state = self
                    .inner
                    .wake
                    .wait_timeout(state, period)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
                continue;
            }
            if let Some((meta, buffer)) = state.pending.take() {
                self.push(&mut state, meta, buffer);
            } else if let (FrameRate::Exact(_), Some((meta, buffer))) =
                (state.fps, state.last.clone())
            {
                let mut meta = meta;
                meta.repeat = true;
                meta.damage = Damage::Unchanged;
                meta.skipped_before = 0;
                meta.pts_ns = clock::monotonic_ns();
                meta.pts_clock = crate::types::PtsClock::Monotonic;
                self.push(&mut state, meta, buffer);
            } else {
                // Nothing to deliver until a frame arrives.
                state = self
                    .inner
                    .wake
                    .wait_timeout(state, period)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
    }

    /// End the session: queued frames stay receivable, the sink ends
    /// `Completed`, `Stopped(reason)` is the last event.
    pub(crate) fn stop(&self, reason: StopReason) {
        {
            let mut state = lock(&self.inner.state);
            if state.stopped && self.inner.sender.is_closed() {
                return;
            }
            state.stopped = true;
            state.pending = None;
            state.last = None;
        }
        self.inner.wake.notify_all();
        self.inner.sender.close();
        self.event(CaptureEvent::Stopped(reason));
        self.inner.events.end();
    }

    /// End the session with `error`: the sink ends `Failed(error)`, then
    /// `Failed` and `Stopped` are emitted.
    pub(crate) fn fail(&self, error: CaptureError) {
        let reason = match error {
            CaptureError::SourceLost => StopReason::SourceLost,
            CaptureError::ConsentRevoked => StopReason::ConsentRevoked,
            _ => StopReason::Failed,
        };
        {
            let mut state = lock(&self.inner.state);
            state.stopped = true;
            state.pending = None;
            state.last = None;
        }
        self.inner.wake.notify_all();
        self.inner.sender.close_with(error.clone());
        match reason {
            StopReason::SourceLost => self.event(CaptureEvent::SourceLost),
            StopReason::ConsentRevoked => self.event(CaptureEvent::ConsentRevoked),
            StopReason::Closed | StopReason::Failed | StopReason::Shutdown => {}
        }
        self.event(CaptureEvent::Failed(error));
        self.event(CaptureEvent::Stopped(reason));
        self.inner.events.end();
    }
}

fn crop_rect(meta: &FrameMeta, crop: Crop) -> Rect {
    let map = OrientationMap {
        size: meta.size,
        orientation: meta.orientation,
    };
    let upright = logical_to_buffer(crop.logical, crop.scale_permille, map.output_size());
    let rect = map.buffer_rect(upright);
    rect.intersect(meta.content_rect).unwrap_or(rect)
}

/// A session's audio output.
#[derive(Debug, Clone)]
pub(crate) struct AudioOut {
    sender: FrameSender<AudioMeta>,
    events: Arc<EventHub>,
    seq: Arc<Mutex<u64>>,
}

impl AudioOut {
    pub(crate) fn new(sender: FrameSender<AudioMeta>, events: Arc<EventHub>) -> Self {
        Self {
            sender,
            events,
            seq: Arc::new(Mutex::new(0)),
        }
    }

    /// Hand a chunk over (`meta.seq` is filled in). `false` once the
    /// sink is gone; a refused chunk (a full `DropNewest` sink) is
    /// counted by the sink and reported as delivered.
    pub(crate) fn deliver(&self, mut meta: AudioMeta, buffer: NativeBuffer) -> bool {
        let mut seq = lock(&self.seq);
        meta.seq = *seq;
        *seq += 1;
        match self.sender.push(meta, buffer) {
            Ok(_) => true,
            Err(err) => !err.is_closed(),
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    pub(crate) fn event(&self, event: CaptureEvent) {
        self.events.emit(event);
    }

    pub(crate) fn stop(&self) {
        self.sender.close();
    }

    pub(crate) fn fail(&self, error: CaptureError) {
        self.sender.close_with(error);
    }
}

/// The content rectangle of `meta`'s buffer showing `crop`.
pub(crate) fn crop_rect_for(meta: &FrameMeta, crop: Crop) -> Rect {
    crop_rect(meta, crop)
}

/// The same for a buffer of `size` needing `orientation`.
pub(crate) fn crop_rect_for_size(
    size: crate::types::Size,
    orientation: crate::types::Orientation,
    crop: Crop,
) -> Rect {
    let map = OrientationMap { size, orientation };
    let upright = logical_to_buffer(crop.logical, crop.scale_permille, map.output_size());
    map.buffer_rect(upright)
}

/// Turn a CPU frame upright (`CopyStage::OrientationUpright`). Frames
/// that are already upright, or not in CPU memory, pass unchanged.
/// `OrientationHandling::Upright`: a turned CPU copy of a rotated frame.
/// A rotated frame the plugin cannot turn (GPU memory, an unreadable
/// layout) ends the session: delivering it unturned would break the
/// requirement silently.
fn turn_upright(
    backend: BackendId,
    meta: FrameMeta,
    buffer: NativeBuffer,
) -> Result<(FrameMeta, NativeBuffer), CaptureError> {
    use crate::pixels::CpuView;
    use crate::types::{CopyStage, Orientation, PlaneLayout, Requirement};
    if meta.orientation.is_upright() {
        return Ok((meta, buffer));
    }
    let unmet = || CaptureError::unsatisfiable(Some(backend), Requirement::Upright);
    let stride = meta.planes.first().map_or(0, |p| p.stride);
    let bytes = buffer.as_bytes().ok_or_else(unmet)?;
    let (size, stride, bytes) = CpuView::new(bytes, meta.size, stride, meta.format)
        .and_then(|view| view.orient(meta.orientation))
        .map_err(|err| {
            tracing::warn!(%backend, %err, "screen-capture: frame cannot be turned upright");
            unmet()
        })?;
    let mut meta = meta;
    meta.size = size;
    meta.content_rect = Rect::from_size(size);
    meta.planes = vec![PlaneLayout {
        offset: 0,
        stride,
        size: u64::from(stride) * u64::from(size.height),
    }];
    meta.orientation = Orientation::UPRIGHT;
    // Damage rectangles were in the old buffer's space.
    meta.damage = Damage::Full;
    meta.copied = Some(CopyStage::OrientationUpright);
    Ok((meta, NativeBuffer::from_bytes(bytes)))
}
