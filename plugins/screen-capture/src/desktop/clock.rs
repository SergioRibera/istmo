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

/// A process-local monotonic clock elsewhere (the pacer's repeats; the
/// Windows backends time frames on QPC).
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
    // A clock this platform cannot read: the frame is as old as its
    // delivery.
    clock_now_ns(clock).map_or(wall, |now| {
        wall.saturating_sub(now.saturating_sub(pts_ns) / 1000)
    })
}

/// `clock` now, in nanoseconds, where this platform can read it.
fn clock_now_ns(clock: PtsClock) -> Option<u64> {
    match clock {
        PtsClock::Monotonic => Some(monotonic_ns()),
        PtsClock::HostTime => host_time_ns(),
        #[cfg(all(windows, feature = "windows-common"))]
        PtsClock::Qpc => Some(super::windows::os::qpc_now_ns()),
        #[cfg(not(all(windows, feature = "windows-common")))]
        PtsClock::Qpc => None,
        PtsClock::MachContinuous | PtsClock::Performance => None,
    }
}

/// The host time clock (`mach_absolute_time`) now, in nanoseconds.
#[cfg(all(target_os = "macos", feature = "macos-sck"))]
fn host_time_ns() -> Option<u64> {
    super::macos::host_time_ns()
}

/// No host time clock on this platform.
#[cfg(not(all(target_os = "macos", feature = "macos-sck")))]
const fn host_time_ns() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How far `clock` (now at `now` ns) can look back: the process-local
    /// clock starts at 0.
    fn back(now: u64) -> u64 {
        now.min(2_000_000_000)
    }

    fn assert_age(now: u64, clock: PtsClock) {
        let back = back(now);
        let fresh = wall_us_for(now, clock);
        let old = wall_us_for(now - back, clock);
        let age = fresh.saturating_sub(old);
        let expected = back / 1000;
        assert!(
            age.abs_diff(expected) <= 100_000,
            "age {age} µs, expected {expected}"
        );
        assert!(wall_now_us().abs_diff(fresh) < 1_000_000);
    }

    #[test]
    fn wall_time_follows_the_frame_age() {
        assert_age(monotonic_ns(), PtsClock::Monotonic);
    }

    #[cfg(all(windows, feature = "windows-common"))]
    #[test]
    fn qpc_frames_follow_their_age_too() {
        assert_age(super::super::windows::os::qpc_now_ns(), PtsClock::Qpc);
    }
}
