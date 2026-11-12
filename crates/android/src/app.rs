use std::sync::OnceLock;

use android_activity::AndroidApp;

static APP: OnceLock<AndroidApp> = OnceLock::new();

pub fn set_android_app(app: AndroidApp) {
    let _ = APP.set(app);
}

#[must_use]
pub fn android_app() -> Option<AndroidApp> {
    APP.get().cloned()
}

/// The `android.app.Activity` instance as a raw JNI `jobject`.
///
/// `android-activity` 0.6 already returns the Java activity here (not
/// an `ANativeActivity*`): a JNI global reference owned by the
/// `AndroidApp`, valid while the stored app is alive — which is the
/// whole process, since [`set_android_app`] keeps it in a `static`.
/// Only the first `AndroidApp` is kept, so after the activity is
/// recreated in the same process the reference still names the first
/// (possibly destroyed) activity instance.
/// Do not wrap it in an owning `GlobalRef` / `AutoLocal`; it is not
/// yours to delete.
#[must_use]
pub fn android_activity_object() -> Option<*mut std::ffi::c_void> {
    let ptr = android_app()?.activity_as_ptr();
    (!ptr.is_null()).then_some(ptr)
}
