//! Per-subscriber frame fan-out shared by the Rust-hosted backends.
//!
//! Every [`Camera::frames`](crate::Camera::frames) subscriber gets its
//! own bounded queue of [`CameraInitConfig::frame_pool_depth`](crate::CameraInitConfig::frame_pool_depth)
//! frames, so each one sees every frame. Publishing never blocks the
//! capture thread: when a subscriber's queue is full its oldest frame is
//! evicted, and the eviction is reported on that subscriber's next
//! delivered frame through [`CameraFrame::dropped_before`] — the same
//! drop-oldest policy the Android and iOS backends use.

use std::sync::Mutex;

use istmo_core::sync::lock;

use crate::CameraFrame;

/// Fan-out point between one capture pipeline and its subscribers.
#[derive(Debug)]
pub(crate) struct FrameFanout {
    depth: usize,
    state: Mutex<FanoutState>,
}

#[derive(Debug, Default)]
struct FanoutState {
    subscribers: Vec<Subscriber>,
    /// Set by [`FrameFanout::close`]; later subscribers get a stream
    /// that ends immediately.
    closed: bool,
}

#[derive(Debug)]
struct Subscriber {
    tx: flume::Sender<CameraFrame>,
    /// The fan-out's own handle on the queue, used to evict the oldest
    /// frame when it is full.
    evict: flume::Receiver<CameraFrame>,
    /// Frames this subscriber lost since its last delivered frame.
    pending_drops: u32,
}

impl Subscriber {
    /// Whether the subscriber still holds its receiver. The fan-out's
    /// eviction handle is the other receiver on the channel.
    fn is_live(&self) -> bool {
        self.tx.receiver_count() > 1
    }

    const fn add_drops(&mut self, count: u32) {
        self.pending_drops = self.pending_drops.saturating_add(count);
    }

    /// Queue `frame`, evicting the oldest queued frames until it fits.
    fn deliver(&mut self, mut frame: CameraFrame) {
        loop {
            frame.dropped_before = self.pending_drops;
            match self.tx.try_send(frame) {
                Ok(()) => {
                    self.pending_drops = 0;
                    return;
                }
                Err(flume::TrySendError::Full(back)) => {
                    frame = back;
                    // `Empty` means the subscriber drained the queue in
                    // the meantime; the retry then fits.
                    if let Ok(evicted) = self.evict.try_recv() {
                        self.add_drops(evicted.dropped_before.saturating_add(1));
                    }
                }
                // Unreachable while `evict` is alive; nothing to deliver to.
                Err(flume::TrySendError::Disconnected(_)) => return,
            }
        }
    }
}

impl FrameFanout {
    /// Fan-out whose subscribers each buffer up to `depth` frames
    /// (at least one).
    pub(crate) fn new(depth: usize) -> Self {
        Self {
            depth: depth.max(1),
            state: Mutex::new(FanoutState::default()),
        }
    }

    /// New subscriber stream. Ends immediately once the fan-out is
    /// closed.
    pub(crate) fn subscribe(&self) -> flume::Receiver<CameraFrame> {
        let mut state = lock(&self.state);
        if state.closed {
            return flume::bounded(0).1;
        }
        let (tx, rx) = flume::bounded(self.depth);
        state.subscribers.push(Subscriber {
            tx,
            evict: rx.clone(),
            pending_drops: 0,
        });
        rx
    }

    /// Deliver `frame` to every live subscriber without blocking.
    /// Subscribers whose receiver was dropped are forgotten.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the guard protects the subscribers being delivered to"
    )]
    pub(crate) fn publish(&self, frame: CameraFrame) {
        let mut state = lock(&self.state);
        state.subscribers.retain(Subscriber::is_live);
        let Some((last, rest)) = state.subscribers.split_last_mut() else {
            return;
        };
        for subscriber in rest {
            subscriber.deliver(frame.clone());
        }
        last.deliver(frame);
    }

    /// Count a frame the pipeline lost before it could be published
    /// (e.g. a failed canvas read). Reported to every subscriber.
    #[cfg_attr(
        not(any(all(target_arch = "wasm32", feature = "web"), test)),
        expect(dead_code, reason = "only the web pump loses frames before publishing")
    )]
    pub(crate) fn record_drop(&self) {
        for subscriber in &mut lock(&self.state).subscribers {
            subscriber.add_drops(1);
        }
    }

    /// End every subscriber stream (after the frames still queued) and
    /// refuse new subscribers.
    pub(crate) fn close(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.subscribers.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::FrameFanout;
    use crate::{CameraFrame, PixelFormat, Resolution};

    fn frame(seq: u64) -> CameraFrame {
        CameraFrame {
            seq,
            timestamp_ns: seq,
            resolution: Resolution {
                width: 1,
                height: 1,
            },
            rotation_degrees: 0,
            pixel_format: PixelFormat::Rgba8,
            planes: Vec::new(),
            bytes: vec![0; 4],
            dropped_before: 0,
        }
    }

    fn drain(rx: &flume::Receiver<CameraFrame>) -> Vec<(u64, u32)> {
        rx.try_iter().map(|f| (f.seq, f.dropped_before)).collect()
    }

    #[test]
    fn every_subscriber_gets_every_frame() {
        let fanout = FrameFanout::new(4);
        let a = fanout.subscribe();
        let b = fanout.subscribe();
        for seq in 0..3 {
            fanout.publish(frame(seq));
        }
        assert_eq!(drain(&a), [(0, 0), (1, 0), (2, 0)]);
        assert_eq!(drain(&b), [(0, 0), (1, 0), (2, 0)]);
    }

    #[test]
    fn full_queue_drops_oldest_and_reports_it() {
        let fanout = FrameFanout::new(2);
        let rx = fanout.subscribe();
        for seq in 0..5 {
            fanout.publish(frame(seq));
        }
        // 0, 1 and 2 were evicted; each eviction is counted on the next
        // frame queued after it.
        assert_eq!(drain(&rx), [(3, 1), (4, 2)]);
        fanout.publish(frame(5));
        assert_eq!(drain(&rx), [(5, 0)]);
    }

    #[test]
    fn slow_subscriber_does_not_starve_fast_one() {
        let fanout = FrameFanout::new(1);
        let slow = fanout.subscribe();
        let fast = fanout.subscribe();
        let mut seen = Vec::new();
        for seq in 0..4 {
            fanout.publish(frame(seq));
            seen.extend(drain(&fast));
        }
        assert_eq!(seen, [(0, 0), (1, 0), (2, 0), (3, 0)]);
        assert_eq!(drain(&slow), [(3, 3)]);
    }

    #[test]
    fn evicted_frame_carries_its_own_drops_forward() {
        let fanout = FrameFanout::new(1);
        let rx = fanout.subscribe();
        fanout.publish(frame(0));
        fanout.record_drop();
        fanout.publish(frame(1)); // evicts 0, queued with dropped_before = 2
        fanout.publish(frame(2)); // evicts 1, inheriting its 2 drops
        assert_eq!(drain(&rx), [(2, 3)]);
    }

    #[test]
    fn dropped_subscriber_is_forgotten() {
        let fanout = FrameFanout::new(2);
        let kept = fanout.subscribe();
        drop(fanout.subscribe());
        fanout.publish(frame(0));
        assert_eq!(drain(&kept), [(0, 0)]);
        assert_eq!(super::lock(&fanout.state).subscribers.len(), 1);
    }

    #[test]
    fn close_ends_streams_after_queued_frames() {
        let fanout = FrameFanout::new(2);
        let rx = fanout.subscribe();
        fanout.publish(frame(0));
        fanout.close();
        assert_eq!(rx.recv().map(|f| f.seq), Ok(0));
        assert!(rx.recv().is_err());
        assert!(fanout.subscribe().recv().is_err());
    }
}
