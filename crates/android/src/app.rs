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

/// Return the Activity `jobject` stashed inside the `ANativeActivity`
/// struct `android-activity` handed us at process start.
///
/// `ndk-context::android_context().context()` returns the process
/// `Application`, not the current Activity — calling `getWindow()` on
/// it throws `NoSuchMethodError`. Reaching the Activity requires
/// reading the `clazz` field on the `ANativeActivity` struct that
/// `AndroidApp::activity_as_ptr()` points at.
///
/// `ANativeActivity` layout from `<android/native_activity.h>`:
///
/// ```c
/// struct ANativeActivity {
///     struct ANativeActivityCallbacks* callbacks;  // offset 0
///     JavaVM* vm;                                   // offset 1
///     JNIEnv* env;                                  // offset 2
///     jobject clazz;                                // offset 3  <-- Activity
///     const char* internalDataPath;
///     ...
/// }
/// ```
///
/// All fields are pointer-sized; `clazz` sits at offset `3 * sizeof(*mut c_void)`.
#[must_use]
pub fn android_activity_object() -> Option<*mut std::ffi::c_void> {
    use std::ffi::c_void;
    let app = android_app()?;
    let ptr = app.activity_as_ptr();
    if ptr.is_null() {
        return None;
    }
    // SAFETY: `activity_as_ptr` returns a pointer at a live
    // `ANativeActivity` struct kept alive by the NDK glue for the
    // entire process lifetime; the `clazz` field at offset 3 is a
    // stable jobject reference.
    unsafe {
        let clazz_ptr = (ptr as *mut *mut c_void).add(3);
        let clazz = *clazz_ptr;
        if clazz.is_null() { None } else { Some(clazz) }
    }
}
