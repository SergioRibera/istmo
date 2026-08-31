//! Small synchronisation helpers used across the crate.
//!
//! We wrap `Mutex::lock` in a helper that transparently recovers a poisoned
//! guard. All mutexes in `istmo-core` protect data that cannot be corrupted
//! by a partial mutation (hash-map inserts / removes, small structs), so a
//! poisoned mutex is only a signal that some other thread panicked — the
//! data itself remains consistent and we prefer forward progress over
//! surfacing an error nobody can act on.

use std::sync::{Mutex, MutexGuard};

// `pub` here is `pub(crate)` in practice because the module itself is private;
// spelling it `pub` avoids the `clippy::redundant_pub_crate` lint, and the
// `unreachable_pub` warning is allowed locally since the function is
// deliberately not re-exported.
#[allow(unreachable_pub)]
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
