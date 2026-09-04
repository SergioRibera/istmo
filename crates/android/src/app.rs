//! Global `AndroidApp` handle used by `#[istmo::mobile_app]`.
//!
//! The generated `android_main` trampoline stashes the handle the NDK
//! glue passed in via [`set_android_app`]; user code retrieves it with
//! [`android_app`] when it is ready to boot its event loop (eframe /
//! winit / iced / whatever). The value is stored in a `OnceLock` so a
//! rehydrated process observes the first handle and later `set` calls
//! are silently ignored.

use std::sync::OnceLock;

use android_activity::AndroidApp;

static APP: OnceLock<AndroidApp> = OnceLock::new();

/// Store the `AndroidApp` handle. Called from the macro-emitted
/// `android_main`. Manual invocation is a no-op after the first `set`.
pub fn set_android_app(app: AndroidApp) {
    let _ = APP.set(app);
}

/// Retrieve the `AndroidApp` handle the NDK glue passed in.
/// Returns `Some` after the process has entered `android_main`;
/// `None` before.
#[must_use]
pub fn android_app() -> Option<AndroidApp> {
    APP.get().cloned()
}
