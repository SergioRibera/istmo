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

