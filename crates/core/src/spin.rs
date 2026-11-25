//! Bounded, adaptive spinning before a thread parks.
//!
//! Waking a parked thread costs 10–30 µs on an idle phone (the scheduler
//! brings a core out of idle, often from a low frequency step), while a
//! partner that answers within a few microseconds is caught by polling.
//! A [`SpinWait`] polls for at most [`MAX_SPIN`] before the caller parks,
//! and adapts per wait site:
//!
//! - a hit stretches the budget to twice what it needed (up to the cap);
//! - a miss halves it, and repeated misses stop spinning for an
//!   exponentially growing number of waits (up to 64) before trying
//!   again with the smallest budget — on a loaded system, where the
//!   partner rarely runs within the window, spinning soon stops;
//! - no thread spins while as many threads spin as the machine has cores
//!   minus one, and a single-core machine never spins.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Longest a wait polls before parking.
pub const MAX_SPIN: Duration = Duration::from_micros(20);

const MAX_NS: u32 = 20_000;
const MIN_NS: u32 = 1_000;
const START_NS: u32 = 5_000;
/// Waits skipped after a run of misses: `1 << misses`, at most this.
const MAX_SKIP: u32 = 64;
/// Polls between two clock reads.
const POLLS_PER_CLOCK: u32 = 8;

static SPINNING: AtomicUsize = AtomicUsize::new(0);
static ENABLED: AtomicBool = AtomicBool::new(true);

/// Threads allowed to spin at once: every core but one.
fn spin_slots() -> usize {
    static SLOTS: OnceLock<usize> = OnceLock::new();
    *SLOTS.get_or_init(|| {
        std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .saturating_sub(1)
    })
}

/// Turn spinning on or off for the whole process (benchmarks compare
/// both). On by default.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

/// Whether [`set_enabled`] left spinning on.
#[must_use]
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The adaptive spin state of one wait site; usually a `static`.
#[derive(Debug)]
pub struct SpinWait {
    /// Current budget in nanoseconds.
    budget_ns: AtomicU32,
    /// Misses in a row.
    misses: AtomicU32,
    /// Waits left to skip without spinning.
    skip: AtomicU32,
}

impl Default for SpinWait {
    fn default() -> Self {
        Self::new()
    }
}

impl SpinWait {
    /// A wait site starting with a 5 µs budget.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            budget_ns: AtomicU32::new(START_NS),
            misses: AtomicU32::new(0),
            skip: AtomicU32::new(0),
        }
    }

    /// Poll `ready` until it yields a value or the budget runs out;
    /// `None` means the caller should park now.
    pub fn spin<T>(&self, mut ready: impl FnMut() -> Option<T>) -> Option<T> {
        if !is_enabled() || !self.take_turn() {
            return None;
        }
        let _slot = SpinSlot::claim()?;
        let budget = Duration::from_nanos(u64::from(self.budget_ns.load(Ordering::Relaxed)));
        let start = Instant::now();
        let mut polls = 0u32;
        loop {
            if let Some(value) = ready() {
                self.hit(start.elapsed());
                return Some(value);
            }
            polls = polls.wrapping_add(1);
            if polls.is_multiple_of(POLLS_PER_CLOCK) && start.elapsed() >= budget {
                self.miss();
                return None;
            }
            std::hint::spin_loop();
        }
    }

    /// `false` while backing off after misses.
    fn take_turn(&self) -> bool {
        let mut left = self.skip.load(Ordering::Relaxed);
        while left > 0 {
            match self.skip.compare_exchange_weak(
                left,
                left - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return false,
                Err(actual) => left = actual,
            }
        }
        true
    }

    fn hit(&self, needed: Duration) {
        self.misses.store(0, Ordering::Relaxed);
        let needed = u32::try_from(needed.as_nanos()).unwrap_or(MAX_NS);
        let current = self.budget_ns.load(Ordering::Relaxed);
        let wanted = needed.saturating_mul(2).clamp(MIN_NS, MAX_NS);
        self.budget_ns
            .store(current.max(wanted).min(MAX_NS), Ordering::Relaxed);
    }

    fn miss(&self) {
        let misses = self
            .misses
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        let halved = self.budget_ns.load(Ordering::Relaxed) / 2;
        if halved >= MIN_NS {
            self.budget_ns.store(halved, Ordering::Relaxed);
            return;
        }
        self.budget_ns.store(MIN_NS, Ordering::Relaxed);
        let skip = 1u32
            .checked_shl(misses.min(6))
            .unwrap_or(MAX_SKIP)
            .min(MAX_SKIP);
        self.skip.store(skip, Ordering::Relaxed);
    }

    /// Current budget (tests, diagnostics).
    #[must_use]
    pub fn budget(&self) -> Duration {
        Duration::from_nanos(u64::from(self.budget_ns.load(Ordering::Relaxed)))
    }
}

/// One of the [`spin_slots`] while held.
struct SpinSlot;

impl SpinSlot {
    fn claim() -> Option<Self> {
        let slots = spin_slots();
        let mut taken = SPINNING.load(Ordering::Relaxed);
        loop {
            if taken >= slots {
                return None;
            }
            match SPINNING.compare_exchange_weak(
                taken,
                taken + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Self),
                Err(actual) => taken = actual,
            }
        }
    }
}

impl Drop for SpinSlot {
    fn drop(&mut self) {
        SPINNING.fetch_sub(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multi_core() -> bool {
        spin_slots() > 0
    }

    #[test]
    fn a_ready_value_is_taken_without_parking() {
        let site = SpinWait::new();
        if multi_core() {
            assert_eq!(site.spin(|| Some(7)), Some(7));
        }
    }

    #[test]
    fn misses_shrink_the_budget_then_back_off() {
        let site = SpinWait::new();
        if !multi_core() {
            assert_eq!(site.spin(|| None::<()>), None);
            return;
        }
        let started = Instant::now();
        assert_eq!(site.spin(|| None::<()>), None);
        assert!(started.elapsed() < Duration::from_millis(5));
        assert!(site.budget() < Duration::from_nanos(u64::from(START_NS)));
        for _ in 0..8 {
            let _ = site.spin(|| None::<()>);
        }
        assert_eq!(site.budget(), Duration::from_nanos(u64::from(MIN_NS)));
        // Backing off: the next wait does not even poll.
        let mut polled = false;
        let _ = site.spin(|| {
            polled = true;
            None::<()>
        });
        assert!(!polled, "a backed-off site parks at once");
    }

    #[test]
    fn hits_grow_the_budget_up_to_the_cap() {
        let site = SpinWait::new();
        if !multi_core() {
            return;
        }
        let target = Instant::now() + Duration::from_micros(4);
        assert_eq!(
            site.spin(|| (Instant::now() >= target).then_some(())),
            Some(())
        );
        assert!(site.budget() <= MAX_SPIN);
        assert!(site.budget() >= Duration::from_micros(5));
    }
}
