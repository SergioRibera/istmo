//! Clocks: `pts_ns` on the producer's clock, `wall_us` computed per
//! frame from it (`wall_now − (clock_now − pts)`, both sampled at
//! delivery), so wall-clock steps and suspend show at once.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::types::PtsClock;

/// `CLOCK_MONOTONIC` now, in nanoseconds.
#[cfg(target_os = "linux")]
#[must_use]
pub(crate) fn monotonic_ns() -> u64 {
    let ts = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    u64::try_from(ts.tv_sec).unwrap_or(0) * 1_000_000_000 + u64::try_from(ts.tv_nsec).unwrap_or(0)
}

/// A process-local monotonic clock elsewhere (no backend uses it yet).
#[cfg(not(target_os = "linux"))]
#[must_use]
pub(crate) fn monotonic_ns() -> u64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    u64::try_from(START.get_or_init(Instant::now).elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// UNIX epoch now, in microseconds.
#[must_use]
pub(crate) fn wall_now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

/// The wall-clock time of a frame captured at `pts_ns` on `clock`.
#[must_use]
pub(crate) fn wall_us_for(pts_ns: u64, clock: PtsClock) -> u64 {
    let wall = wall_now_us();
    match clock {
        PtsClock::Monotonic => {
            let age_us = monotonic_ns().saturating_sub(pts_ns) / 1000;
            wall.saturating_sub(age_us)
        }
        // No desktop backend on these clocks yet: the frame is as old as
        // its delivery.
        PtsClock::Qpc | PtsClock::MachContinuous | PtsClock::HostTime | PtsClock::Performance => {
            wall
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_time_follows_the_frame_age() {
        let now = monotonic_ns();
        let fresh = wall_us_for(now, PtsClock::Monotonic);
        let old = wall_us_for(now.saturating_sub(2_000_000_000), PtsClock::Monotonic);
        let age = fresh.saturating_sub(old);
        assert!((1_900_000..=2_100_000).contains(&age), "age {age} µs");
        assert!(wall_now_us().abs_diff(fresh) < 1_000_000);
    }
}
