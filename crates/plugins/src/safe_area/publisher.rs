//! Rust-side safe-area publisher.
//!
//! On iOS / tvOS / visionOS [`SafeAreaPublisher::tick`] reads the
//! `safeAreaInsets` of the key window of the foreground-active
//! `UIWindowScene`. `UIKit` may only be touched from the main thread, so
//! `tick` must be called there (from the UI framework's frame callback);
//! called from any other thread it publishes nothing. Elsewhere `tick`
//! is a no-op and the host publishes the insets itself.

#[cfg(any(target_os = "ios", target_os = "tvos", target_os = "visionos"))]
use super::EdgeInsets;
use super::SafeAreaInsets;
use crate::early::LatestPublisher;

/// Publishes the insets of the app's key window.
pub type SafeAreaPublisher = LatestPublisher<SafeAreaInsets>;

impl LatestPublisher<SafeAreaInsets> {
    /// Read the current insets and publish them if they changed. Call on
    /// the main thread (see the [module docs](self)).
    pub fn tick(&mut self) {
        if let Some(next) = sample() {
            self.publish_if_changed(next);
        }
    }
}

#[allow(
    clippy::missing_const_for_fn,
    reason = "const only on targets without a UIKit sampler"
)]
fn sample() -> Option<SafeAreaInsets> {
    #[cfg(any(target_os = "ios", target_os = "tvos", target_os = "visionos"))]
    {
        let Some(mtm) = objc2::MainThreadMarker::new() else {
            tracing::debug!(target: "istmo.safe_area", "tick off the main thread; skipped");
            return None;
        };
        ios::read_key_window_insets(mtm)
    }
    #[cfg(not(any(target_os = "ios", target_os = "tvos", target_os = "visionos")))]
    {
        None
    }
}

#[cfg(any(target_os = "ios", target_os = "tvos", target_os = "visionos"))]
#[allow(unsafe_code)]
mod ios {
    use objc2::MainThreadMarker;
    use objc2::msg_send;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};

    use super::{EdgeInsets, SafeAreaInsets};

    /// `UISceneActivationStateForegroundActive`.
    const FOREGROUND_ACTIVE: isize = 0;

    /// `_mtm` proves the caller is on the main thread, which every UIKit
    /// message below requires.
    pub(super) fn read_key_window_insets(_mtm: MainThreadMarker) -> Option<SafeAreaInsets> {
        // `connectedScenes.allObjects` and `windows` hand out
        // autoreleased collections; drain them before returning.
        autoreleasepool(|_| {
            let window = key_window()?;
            // SAFETY: `window` is a live `UIWindow` (retained by its
            // scene for at least this pool), messaged on the main thread.
            let insets: UIEdgeInsets = unsafe { msg_send![window, safeAreaInsets] };
            #[allow(
                clippy::cast_possible_truncation,
                reason = "insets are a few hundred points; f32 holds them exactly enough"
            )]
            let edges = EdgeInsets {
                top: insets.top as f32,
                right: insets.right as f32,
                bottom: insets.bottom as f32,
                left: insets.left as f32,
            };
            Some(SafeAreaInsets {
                system_bars: edges,
                ime: EdgeInsets::ZERO,
                display_cutout: EdgeInsets::ZERO,
            })
        })
    }

    /// Key window of the first foreground-active window scene. Replaces
    /// `UIApplication.keyWindow`, deprecated since iOS 13 and wrong with
    /// several scenes.
    fn key_window() -> Option<*mut AnyObject> {
        let app_class = AnyClass::get(c"UIApplication")?;
        let scene_class = AnyClass::get(c"UIWindowScene")?;
        // SAFETY: main thread (see caller); every receiver is null-checked
        // before it is messaged, indices stay below `count`, and the
        // selectors exist on iOS 13+ with the declared argument and
        // return types.
        unsafe {
            let app: *mut AnyObject = msg_send![app_class, sharedApplication];
            if app.is_null() {
                return None;
            }
            let scenes: *mut AnyObject = msg_send![app, connectedScenes];
            if scenes.is_null() {
                return None;
            }
            let scenes: *mut AnyObject = msg_send![scenes, allObjects];
            let count: usize = msg_send![scenes, count];
            for i in 0..count {
                let scene: *mut AnyObject = msg_send![scenes, objectAtIndex: i];
                let is_window_scene: bool = msg_send![scene, isKindOfClass: scene_class];
                let state: isize = msg_send![scene, activationState];
                if !is_window_scene || state != FOREGROUND_ACTIVE {
                    continue;
                }
                let windows: *mut AnyObject = msg_send![scene, windows];
                if windows.is_null() {
                    continue;
                }
                let n: usize = msg_send![windows, count];
                for j in 0..n {
                    let window: *mut AnyObject = msg_send![windows, objectAtIndex: j];
                    let is_key: bool = msg_send![window, isKeyWindow];
                    if is_key {
                        return Some(window);
                    }
                }
            }
            None
        }
    }

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
