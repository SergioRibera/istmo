//! Locking helpers shared by the runtime and plugin crates.

use std::sync::{Mutex, MutexGuard};

/// Lock `mutex`, recovering the guard if a previous holder panicked.
///
/// Poisoning only records that some thread panicked while holding the
/// lock. istmo's critical sections are short map or slot mutations
/// that leave the data consistent at every step, so forward progress
/// beats turning one panic into a cascade of unactionable errors. Use
/// this instead of `mutex.lock().expect(..)` for state with the same
/// property.
pub fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
