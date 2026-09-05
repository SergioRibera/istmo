//! Rust-side safe-area publisher.
//!
//! iOS: walks `UIApplication.shared.connectedScenes` to find the key
//! `UIWindow`, reads its `safeAreaInsets` on each
//! [`SafeAreaPublisher::tick`] call and publishes to
//! [`super::SAFE_AREA_CHANNEL`] whenever the value moves. No winit
//! handle plumbing — the app is guaranteed to have exactly one window
//! (winit's iOS backend creates one, eframe hosts inside it), so
//! `keyWindow` is unambiguous.
//!
//! Non-Apple targets: the type is inert. Android's Kotlin
//! `IstmoRuntime.publishSafeArea` still owns the `WindowInsets`
//! pipeline; a JNI-driven Rust publisher lands as a follow-up.
//!
//! Wire encoding matches the Kotlin publisher: 12 `f32` fields in
//! declaration order — the shape bincode 2 gives `SafeAreaInsets`
//! verbatim.

use std::sync::{Arc, Weak};

use istmo_core::{IstmoError, Runtime, codec};

// `EdgeInsets` referenced by the `ios::` / `android::` sub-modules
// via their own `use super::…` imports — `super` there resolves to
// this publisher module, so the type has to be in scope here.
#[cfg(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
))]
use super::EdgeInsets;
use super::{SAFE_AREA_CHANNEL, SafeAreaInsets};

const PLUGIN_ID: &str = "istmo.safe_area";

/// Holds the runtime handle needed to sample the current safe-area
/// insets and publish updates.
#[derive(Debug)]
pub struct SafeAreaPublisher {
    runtime: Weak<Runtime>,
    last: Option<SafeAreaInsets>,
}

impl SafeAreaPublisher {
    /// Install a Rust-side publisher for `istmo.safe_area`.
    ///
    /// # Errors
    /// Returns [`IstmoError::PluginNotDeclared`] when the runtime
    /// enforces declarations and `istmo.safe_area` is missing from the
    /// `plugins:` list.
    pub fn install(runtime: &Arc<Runtime>) -> Result<Self, IstmoError> {
        runtime.check_declared(PLUGIN_ID)?;
        Ok(Self {
            runtime: Arc::downgrade(runtime),
            last: None,
        })
    }

    /// Sample the platform state and publish an updated
    /// [`SafeAreaInsets`] whenever the value moves.
    ///
    /// Call from the app's per-frame update or from a debounced timer.
    /// Publishing an unchanged value is skipped so downstream consumers
    /// don't wake up on every frame.
    pub fn tick(&mut self) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let Some(next) = self.sample() else {
            return;
        };
        if self.last.as_ref() == Some(&next) {
            return;
        }
        match codec::encode(&next) {
            Ok(bytes) => {
                runtime.publish_early_latest(SAFE_AREA_CHANNEL, bytes);
                self.last = Some(next);
            }
            Err(err) => {
                tracing::warn!(target = "istmo.safe_area", ?err, "encode failed");
            }
        }
    }

    /// Read the current insets from the platform. Returns `None` when
    /// the target has no publisher-side implementation (Android and
    /// desktop today).
    fn sample(&self) -> Option<SafeAreaInsets> {
        #[cfg(any(target_os = "ios", target_os = "tvos", target_os = "visionos"))]
        {
            ios::read_key_window_insets()
        }
        #[cfg(target_os = "android")]
        {
            android::read_window_insets()
        }
        #[cfg(not(any(
            target_os = "ios",
            target_os = "tvos",
            target_os = "visionos",
            target_os = "android",
        )))]
        {
            None
        }
    }
}

#[cfg(target_os = "android")]
#[allow(unsafe_code)]
mod android {
    //! JNI-driven read of `WindowInsetsCompat` from the process
    //! Activity.
    //!
    //! Flow per tick:
    //!
    //! 1. `ndk_context::android_context()` — set by `android-activity`
    //!    at process start — yields the `JavaVM` and current activity
    //!    `jobject`.
    //! 2. Attach the current thread as a JNI daemon.
    //! 3. Read `activity.getWindow().getDecorView().getRootWindowInsets()`.
    //! 4. Extract the three inset groups (`systemBars()`, `ime()`,
    //!    `displayCutout()`) via `WindowInsetsCompat.toWindowInsetsCompat(...)`.
    //! 5. Divide by `displayMetrics.density` so the returned values are
    //!    in logical dp — same shape the Kotlin publisher shipped.
    //!
    //! Returns `None` on any JNI failure — the caller keeps the last
    //! published snapshot rather than crashing.
    //!
    //! `WindowInsetsCompat` requires `androidx.core:core` on the app's
    //! Gradle classpath. Every istmo demo already pulls it in.

    use jni::JavaVM;
    use jni::objects::{JObject, JValue};
    use jni::signature::{Primitive, ReturnType};

    use super::{EdgeInsets, SafeAreaInsets};

    pub(super) fn read_window_insets() -> Option<SafeAreaInsets> {
        let ctx = ndk_context::android_context();
        // SAFETY: `ndk_context::android_context()` returns valid
        // pointers once `android-activity` has run, which is before any
        // istmo plugin call can fire.
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
        let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
        let mut env = vm.attach_current_thread_as_daemon().ok()?;

        let density = display_density(&mut env, &activity)?;
        let decor = decor_view(&mut env, &activity)?;
        let insets = root_window_insets(&mut env, &decor)?;
        let compat = to_compat(&mut env, &insets)?;
        let bars = read_group(&mut env, &compat, "systemBars", density)?;
        let ime = read_group(&mut env, &compat, "ime", density).unwrap_or(EdgeInsets::ZERO);
        let cutout =
            read_group(&mut env, &compat, "displayCutout", density).unwrap_or(EdgeInsets::ZERO);
        Some(SafeAreaInsets {
            system_bars: bars,
            ime,
            display_cutout: cutout,
        })
    }

    fn display_density(env: &mut jni::JNIEnv, activity: &JObject) -> Option<f32> {
        let res = env
            .call_method(activity, "getResources", "()Landroid/content/res/Resources;", &[])
            .ok()?
            .l()
            .ok()?;
        let metrics = env
            .call_method(
                &res,
                "getDisplayMetrics",
                "()Landroid/util/DisplayMetrics;",
                &[],
            )
            .ok()?
            .l()
            .ok()?;
        env.get_field(&metrics, "density", "F").ok()?.f().ok()
    }

    fn decor_view<'l>(
        env: &mut jni::JNIEnv<'l>,
        activity: &JObject<'l>,
    ) -> Option<JObject<'l>> {
        let window = env
            .call_method(activity, "getWindow", "()Landroid/view/Window;", &[])
            .ok()?
            .l()
            .ok()?;
        env.call_method(&window, "getDecorView", "()Landroid/view/View;", &[])
            .ok()?
            .l()
            .ok()
    }

    fn root_window_insets<'l>(
        env: &mut jni::JNIEnv<'l>,
        decor: &JObject<'l>,
    ) -> Option<JObject<'l>> {
        env.call_method(
            decor,
            "getRootWindowInsets",
            "()Landroid/view/WindowInsets;",
            &[],
        )
        .ok()?
        .l()
        .ok()
    }

    /// Wrap a raw `android.view.WindowInsets` into a
    /// `androidx.core.view.WindowInsetsCompat` — the compat class
    /// exposes typed accessors (`systemBars()`, `ime()`, ...) that
    /// don't exist on the raw platform class until API 30+.
    fn to_compat<'l>(
        env: &mut jni::JNIEnv<'l>,
        insets: &JObject<'l>,
    ) -> Option<JObject<'l>> {
        env.call_static_method(
            "androidx/core/view/WindowInsetsCompat",
            "toWindowInsetsCompat",
            "(Landroid/view/WindowInsets;)Landroidx/core/view/WindowInsetsCompat;",
            &[JValue::Object(insets)],
        )
        .ok()?
        .l()
        .ok()
    }

    /// Extract a single inset group (`systemBars`, `ime`,
    /// `displayCutout`) and convert to logical dp via the density.
    fn read_group(
        env: &mut jni::JNIEnv,
        compat: &JObject,
        group: &str,
        density: f32,
    ) -> Option<EdgeInsets> {
        // `Type.<group>()` is the group id constant.
        let type_id = env
            .call_static_method(
                "androidx/core/view/WindowInsetsCompat$Type",
                group,
                "()I",
                &[],
            )
            .ok()?
            .i()
            .ok()?;
        let insets_obj = env
            .call_method(
                compat,
                "getInsets",
                "(I)Landroidx/core/graphics/Insets;",
                &[JValue::Int(type_id)],
            )
            .ok()?
            .l()
            .ok()?;
        let top = env.get_field(&insets_obj, "top", "I").ok()?.i().ok()? as f32;
        let right = env.get_field(&insets_obj, "right", "I").ok()?.i().ok()? as f32;
        let bottom = env
            .get_field(&insets_obj, "bottom", "I")
            .ok()?
            .i()
            .ok()? as f32;
        let left = env.get_field(&insets_obj, "left", "I").ok()?.i().ok()? as f32;
        Some(EdgeInsets {
            top: top / density,
            right: right / density,
            bottom: bottom / density,
            left: left / density,
        })
    }

    // Suppress an "unused import" warning when the Primitive / ReturnType
    // helpers turn out to be needed only in code paths a future extension
    // adds.
    #[allow(dead_code)]
    fn _keep_types(_: Primitive, _: ReturnType) {}
}

#[cfg(any(target_os = "ios", target_os = "tvos", target_os = "visionos"))]
#[allow(unsafe_code)]
mod ios {
    //! Direct UIKit reads via objc2.
    //!
    //! Walks `UIApplication.sharedApplication.connectedScenes`, finds
    //! the first `UIWindowScene` with a key `UIWindow`, and reads
    //! `safeAreaInsets`. Values come in points, matching egui's
    //! logical-pixel model.
    //!
    //! `additionalSafeAreaInsets` / IME — iOS updates the effective
    //! `safeAreaInsets` when the keyboard slides up via the automatic
    //! keyboard layout guide, so a poll-driven reader captures IME
    //! insets without a dedicated `UIKeyboardWillShow` observer.

    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    use super::{EdgeInsets, SafeAreaInsets};

    /// Read insets from the current key window; `None` when no window
    /// is attached yet (very early in the process lifetime, before the
    /// first frame renders).
    pub(super) fn read_key_window_insets() -> Option<SafeAreaInsets> {
        let view_addr = key_window_view_addr()?;
        // SAFETY: `view_addr` came from a message send on a live
        // `UIWindow` — the returned view is retained by the window,
        // which UIKit keeps alive for the process lifetime.
        let insets: UIEdgeInsets = unsafe {
            let view = view_addr as *mut AnyObject;
            msg_send![view, safeAreaInsets]
        };
        Some(SafeAreaInsets {
            system_bars: EdgeInsets {
                top: insets.top as f32,
                right: insets.right as f32,
                bottom: insets.bottom as f32,
                left: insets.left as f32,
            },
            ime: EdgeInsets::ZERO,
            display_cutout: EdgeInsets::ZERO,
        })
    }

    /// Fetch `UIApplication.sharedApplication.keyWindow` as a raw
    /// pointer, cast to `usize` so the surrounding code stays free of
    /// non-`Send` pointer types.
    fn key_window_view_addr() -> Option<usize> {
        // SAFETY: `UIApplication` is guaranteed to exist inside a
        // running iOS app; every message send below returns nil / null
        // safely when the receiver is missing, and we check each step.
        unsafe {
            let cls = AnyClass::get(c"UIApplication")?;
            let app: *mut AnyObject = msg_send![cls, sharedApplication];
            if app.is_null() {
                return None;
            }
            let window: *mut AnyObject = msg_send![app, keyWindow];
            if window.is_null() {
                return None;
            }
            Some(window as usize)
        }
    }

    /// Matches UIKit's `UIEdgeInsets` layout — order and CGFloat width.
    /// Build targets `aarch64-apple-ios*` where CGFloat is `f64`.
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    struct UIEdgeInsets {
        top: f64,
        left: f64,
        bottom: f64,
        right: f64,
    }

    // SAFETY: The `#[repr(C)]` layout above is byte-for-byte identical
    // to `UIEdgeInsets` in `UIGeometry.h`.
    unsafe impl objc2::Encode for UIEdgeInsets {
        const ENCODING: objc2::Encoding = objc2::Encoding::Struct(
            "UIEdgeInsets",
            &[
                objc2::Encoding::Double,
                objc2::Encoding::Double,
                objc2::Encoding::Double,
                objc2::Encoding::Double,
            ],
        );
    }
}
