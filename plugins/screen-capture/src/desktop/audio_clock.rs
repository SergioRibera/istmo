//! The audio chunk clock: `wall_us` follows the sample count from an
//! anchor, so deltas are exact in 1 / sample-rate ticks, and re-anchors
//! to "now" when the counted time strays from the wall clock (the sink
//! went idle or suspended, an xrun, or the sound card's clock drifting
//! against the system clock, ~50 ppm ≈ 180 ms / h). Shared by every
//! platform's audio backend.

#![cfg_attr(
    not(any(
        all(target_os = "linux", feature = "linux-audio"),
        all(windows, feature = "windows-audio")
    )),
    allow(dead_code, reason = "used by the platform audio backends only")
)]

/// Largest |counted − wall| tolerated before re-anchoring. Delivery
/// jitter is well under one ~21 ms quantum; 100 ms is far above that and
/// below the staleness cuts consumers apply (ferricast's HLS segmenter
/// drops audio older than 300 ms).
pub(crate) const REANCHOR_THRESHOLD_US: u64 = 100_000;

/// The clock of one audio stream.
#[derive(Debug, Default)]
pub(crate) struct SampleClock {
    anchor_us: Option<u64>,
    frames_since_anchor: u64,
}

/// The time of one chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub(crate) wall_us: u64,
    /// `Some(counted − wall)` in µs when this chunk re-anchored the clock
    /// (positive: the counted time was ahead of the wall clock).
    pub(crate) reanchored_by_us: Option<i64>,
}

impl SampleClock {
    /// Time a chunk of `frames` that arrived at `now_us` (UNIX epoch µs),
    /// then advance past it.
    pub(crate) fn stamp(&mut self, frames: u64, rate: u32, now_us: u64) -> Stamp {
        let rate = u64::from(rate.max(1));
        let mut reanchored_by_us = None;
        let wall_us = if let Some(anchor) = self.anchor_us {
            let counted =
                anchor.saturating_add(self.frames_since_anchor.saturating_mul(1_000_000) / rate);
            let drift = i128::from(counted) - i128::from(now_us);
            if drift.unsigned_abs() > u128::from(REANCHOR_THRESHOLD_US) {
                reanchored_by_us = Some(i64::try_from(drift).unwrap_or(if drift > 0 {
                    i64::MAX
                } else {
                    i64::MIN
                }));
                self.anchor_us = Some(now_us);
                self.frames_since_anchor = 0;
                now_us
            } else {
                counted
            }
        } else {
            self.anchor_us = Some(now_us);
            now_us
        };
        self.frames_since_anchor = self.frames_since_anchor.saturating_add(frames);
        Stamp {
            wall_us,
            reanchored_by_us,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    /// 1024 frames at 48 kHz.
    const CHUNK: u64 = 1024;
    const CHUNK_US: u64 = 21_333;

    #[test]
    fn first_chunk_anchors_to_now() {
        let mut clock = SampleClock::default();
        let stamp = clock.stamp(CHUNK, RATE, 5_000_000);
        assert_eq!(stamp.wall_us, 5_000_000);
        assert_eq!(stamp.reanchored_by_us, None);
    }

    #[test]
    fn steady_stream_follows_sample_count_not_jittery_wall() {
        let mut clock = SampleClock::default();
        clock.stamp(CHUNK, RATE, 1_000_000);
        // Arrives 3 ms late: the time still comes from the sample count.
        let stamp = clock.stamp(CHUNK, RATE, 1_000_000 + CHUNK_US + 3_000);
        assert_eq!(stamp.wall_us, 1_000_000 + CHUNK_US);
        assert_eq!(stamp.reanchored_by_us, None);
    }

    #[test]
    fn gap_reanchors_to_now() {
        let mut clock = SampleClock::default();
        clock.stamp(CHUNK, RATE, 1_000_000);
        // The sink went idle for 2 s.
        let now = 3_000_000;
        let stamp = clock.stamp(CHUNK, RATE, now);
        assert_eq!(stamp.wall_us, now);
        assert!(stamp.reanchored_by_us.is_some_and(|d| d < 0));
        let next = clock.stamp(CHUNK, RATE, now + CHUNK_US);
        assert_eq!(next.wall_us, now + CHUNK_US);
        assert_eq!(next.reanchored_by_us, None);
    }

    #[test]
    fn counted_clock_ahead_of_wall_reanchors() {
        let mut clock = SampleClock::default();
        clock.stamp(48_000 * 10, RATE, 0);
        // 10 s of samples delivered after only 9.8 s of wall time.
        let stamp = clock.stamp(CHUNK, RATE, 9_800_000);
        assert_eq!(stamp.wall_us, 9_800_000);
        assert_eq!(stamp.reanchored_by_us, Some(200_000));
    }

    #[test]
    fn drift_within_threshold_is_tolerated() {
        let mut clock = SampleClock::default();
        clock.stamp(48_000 * 10, RATE, 0);
        let stamp = clock.stamp(CHUNK, RATE, 10_000_000 + REANCHOR_THRESHOLD_US);
        assert_eq!(stamp.wall_us, 10_000_000);
        assert_eq!(stamp.reanchored_by_us, None);
    }
}
