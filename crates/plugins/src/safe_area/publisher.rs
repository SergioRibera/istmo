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
        #[cfg(not(any(target_os = "ios", target_os = "tvos", target_os = "visionos")))]
        {
            None
        }
    }
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
    pub fn read_key_window_insets() -> Option<SafeAreaInsets> {
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
