//! The host's [`Wakeup`]: one `setTimeout` at a time, one callback
//! reused for every arm (no closure per arm).
//!
//! Waits are capped at [`MAX_WAIT_MS`]: browsers fire long timers late
//! after a sleep and throttle background tabs, so the host re-reads the
//! clock at least that often.

#![expect(
    clippy::significant_drop_tightening,
    reason = "guards live in short blocks that end right after their last use; the lint asks for explicit drops there"
)]

use std::sync::Mutex;

use istmo_core::sync::lock;
use js_sys::Function;
use send_wrapper::SendWrapper;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

use super::js;
use crate::schedule::Wakeup;

/// Longest single wait.
pub(super) const MAX_WAIT_MS: i64 = 30_000;

/// The armed timeout: its deadline and `setTimeout`'s handle (a number
/// in browsers, an object in Node).
struct Armed {
    deadline: i64,
    handle: SendWrapper<JsValue>,
}

/// A `setTimeout` wake-up calling `tick`.
pub(super) struct Timeout {
    callback: SendWrapper<Closure<dyn FnMut()>>,
    armed: Mutex<Option<Armed>>,
    clock: fn() -> i64,
}

impl std::fmt::Debug for Timeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timeout")
            .field("armed", &lock(&self.armed).as_ref().map(|a| a.deadline))
            .finish_non_exhaustive()
    }
}

impl Timeout {
    pub(super) fn new(tick: impl FnMut() + 'static, clock: fn() -> i64) -> Self {
        Self {
            callback: SendWrapper::new(Closure::new(tick)),
            armed: Mutex::new(None),
            clock,
        }
    }

    /// The fired timeout is no longer armed (called first in `tick`).
    pub(super) fn fired(&self) {
        *lock(&self.armed) = None;
    }

    fn clear(handle: &JsValue) {
        let _cleared = js::call(&js::global(), "clearTimeout", std::slice::from_ref(handle));
    }
}

impl Wakeup for Timeout {
    fn arm(&self, deadline_ms: Option<i64>) {
        let mut armed = lock(&self.armed);
        if armed.as_ref().map(|a| a.deadline) == deadline_ms {
            return;
        }
        if let Some(old) = armed.take() {
            Self::clear(&old.handle);
        }
        let Some(deadline) = deadline_ms else {
            return;
        };
        let wait = deadline
            .saturating_sub((self.clock)())
            .clamp(0, MAX_WAIT_MS);
        let callback: &Function = self.callback.as_ref().unchecked_ref();
        #[expect(
            clippy::cast_precision_loss,
            reason = "wait is at most MAX_WAIT_MS, exact in f64"
        )]
        let wait = JsValue::from_f64(wait as f64);
        match js::call(
            &js::global(),
            "setTimeout",
            &[callback.clone().into(), wait],
        ) {
            Ok(handle) => {
                *armed = Some(Armed {
                    deadline,
                    handle: SendWrapper::new(handle),
                });
            }
            Err(err) => {
                tracing::warn!(err = %js::message(&err), "notifications: setTimeout failed");
            }
        }
    }
}

impl Drop for Timeout {
    fn drop(&mut self) {
        let old = lock(&self.armed).take();
        if let Some(old) = old {
            Self::clear(&old.handle);
        }
    }
}
