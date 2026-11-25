//! Thread hand-off costs with no istmo involved: what every hop of a
//! call pays at least once.
//!
//! Each function returns one round trip per sample (caller → partner →
//! caller), so a one-way wake is about half a sample.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::sys::now_ns;

fn partner(name: &str, body: impl FnOnce() + Send + 'static) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
        .expect("invariant: the bench spawns a handful of threads")
}

/// `flume` bounded(1) ping-pong: the channel the runtime uses for its
/// outbound queue and stream items. `spin_us > 0` polls `try_recv` for
/// that long before blocking, on both sides.
#[must_use]
pub fn flume_ping_pong(samples: usize, spin_us: u64) -> Vec<u64> {
    let (to_partner, partner_rx) = flume::bounded::<u64>(1);
    let (to_caller, caller_rx) = flume::bounded::<u64>(1);
    let window_ns = spin_us * 1_000;
    let handle = partner("bench-partner", move || {
        while let Some(v) = recv_spin(&partner_rx, window_ns) {
            if to_caller.send(v).is_err() {
                break;
            }
        }
    });
    let mut out = Vec::with_capacity(samples);
    for i in 0..samples as u64 {
        let t0 = now_ns();
        let _ = to_partner.send(i);
        let _ = recv_spin(&caller_rx, window_ns);
        out.push(now_ns() - t0);
    }
    drop(to_partner);
    let _ = handle.join();
    out
}

fn recv_spin<T>(rx: &flume::Receiver<T>, spin_ns: u64) -> Option<T> {
    if spin_ns > 0 {
        let start = now_ns();
        while now_ns() - start < spin_ns {
            match rx.try_recv() {
                Ok(v) => return Some(v),
                Err(flume::TryRecvError::Disconnected) => return None,
                Err(flume::TryRecvError::Empty) => std::hint::spin_loop(),
            }
        }
    }
    rx.recv().ok()
}

/// `Mutex` + `Condvar` ping-pong.
#[must_use]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the partner holds the lock between waits by design; `wait` releases it"
)]
pub fn condvar_ping_pong(samples: usize) -> Vec<u64> {
    // turn: 0 = partner's move, 1 = caller's move, 2 = stop.
    let shared = Arc::new((Mutex::new(1u8), Condvar::new()));
    let remote = Arc::clone(&shared);
    let handle = partner("bench-partner", move || {
        let (lock, cv) = &*remote;
        let mut turn = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            while *turn == 1 {
                turn = cv
                    .wait(turn)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if *turn == 2 {
                return;
            }
            *turn = 1;
            cv.notify_all();
        }
    });
    let (lock, cv) = &*shared;
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        let t0 = now_ns();
        let mut turn = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *turn = 0;
        cv.notify_all();
        while *turn == 0 {
            turn = cv
                .wait(turn)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        drop(turn);
        out.push(now_ns() - t0);
    }
    *lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = 2;
    cv.notify_all();
    let _ = handle.join();
    out
}

fn futex_wait(word: &AtomicU32, expected: u32) {
    // SAFETY: `word` is a live, aligned u32 for the whole call; a null
    // timeout waits until woken or the value differs.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word.as_ptr(),
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            expected,
            std::ptr::null::<libc::timespec>(),
        );
    }
}

fn futex_wake(word: &AtomicU32) {
    // SAFETY: `word` is a live, aligned u32; waking has no other effect.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            word.as_ptr(),
            libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
            1,
        );
    }
}

/// Raw futex ping-pong: the floor any blocking hand-off pays.
#[must_use]
pub fn futex_ping_pong(samples: usize) -> Vec<u64> {
    // Even: the partner's move; odd: the caller's; u32::MAX: stop.
    let word = Arc::new(AtomicU32::new(1));
    let remote = Arc::clone(&word);
    let handle = partner("bench-partner", move || {
        loop {
            let v = remote.load(Ordering::Acquire);
            if v == u32::MAX {
                return;
            }
            if v % 2 == 1 {
                futex_wait(&remote, v);
                continue;
            }
            remote.store(v + 1, Ordering::Release);
            futex_wake(&remote);
        }
    });
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        let t0 = now_ns();
        let mine = word.load(Ordering::Acquire) + 1;
        word.store(mine, Ordering::Release);
        futex_wake(&word);
        loop {
            let v = word.load(Ordering::Acquire);
            if v != mine {
                break;
            }
            futex_wait(&word, v);
        }
        out.push(now_ns() - t0);
    }
    word.store(u32::MAX, Ordering::Release);
    futex_wake(&word);
    let _ = handle.join();
    out
}

/// What `DispatchMode::Isolated` pays per call: a named OS thread
/// running `pollster::block_on`. Returns (spawn → body running,
/// spawn → joined) per sample.
#[must_use]
pub fn thread_spawn(samples: usize) -> (Vec<u64>, Vec<u64>) {
    let mut started = Vec::with_capacity(samples);
    let mut joined = Vec::with_capacity(samples);
    for _ in 0..samples {
        let t0 = now_ns();
        let handle = std::thread::Builder::new()
            .name("bench-spawned".to_owned())
            .spawn(|| pollster::block_on(async { now_ns() }))
            .expect("invariant: one short-lived thread at a time");
        let body = handle.join().unwrap_or(t0);
        let t2 = now_ns();
        started.push(body.saturating_sub(t0));
        joined.push(t2 - t0);
    }
    (started, joined)
}

/// Back-to-back clock reads: the timer's own resolution and cost.
#[must_use]
pub fn clock_overhead(samples: usize) -> Vec<u64> {
    (0..samples)
        .map(|_| {
            let t0 = now_ns();
            now_ns() - t0
        })
        .collect()
}

/// Sleep long enough for a CPU to go idle between phases.
pub fn settle() {
    std::thread::sleep(Duration::from_millis(50));
}
