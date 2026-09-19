use std::sync::{Arc, Weak};

use istmo_core::{IstmoError, Runtime, codec};

#[cfg(any(target_os = "ios", target_os = "tvos", target_os = "visionos"))]
use super::EdgeInsets;
use super::{SAFE_AREA_CHANNEL, SafeAreaInsets};

const PLUGIN_ID: &str = "istmo.safe_area";

#[derive(Debug)]
pub struct SafeAreaPublisher {
    runtime: Weak<Runtime>,
    last: Option<SafeAreaInsets>,
}

impl SafeAreaPublisher {

    pub fn install(runtime: &Arc<Runtime>) -> Result<Self, IstmoError> {
        runtime.check_declared(PLUGIN_ID)?;
        Ok(Self {
            runtime: Arc::downgrade(runtime),
            last: None,
        })
    }

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

    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    use super::{EdgeInsets, SafeAreaInsets};

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

