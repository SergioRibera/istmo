//! Rust-side safe-area publisher driven by a `winit`-owned window.
//!
//! On iOS the plugin resolves the underlying `UIView` via
//! `raw-window-handle`, walks to its `UIWindow`, samples
//! `safeAreaInsets` on each [`SafeAreaPublisher::tick`] call and
//! publishes to [`super::SAFE_AREA_CHANNEL`] whenever the value moves.
//! Poll-driven rather than notification-driven so the whole flow lives
//! on the same thread as the caller's event loop; no objc2 delegate
//! lifetimes to chase.
//!
//! On other platforms the type is a no-op — Android's Kotlin publisher
//! is still the recommended path (winit's `AndroidApp` exposes the
//! `ActivityRef` for a JNI implementation but that lands as a follow-up
//! once the demo actually needs a Rust-side Android publisher).
//!
//! Wire encoding matches the Kotlin publisher: 12 `f32` fields in
//! declaration order (system_bars top/right/bottom/left, then `ime`,
//! then `display_cutout`), no length prefix — the shape bincode 2 gives
//! `SafeAreaInsets` verbatim.

use std::sync::{Arc, Weak};

use istmo_core::{IstmoError, Runtime, codec};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::{SAFE_AREA_CHANNEL, SafeAreaInsets};

// SafeArea plugin id — same string the Kotlin / Swift publishers use.
// Kept as a local constant so the publisher does not have to bring the
// `Plugin` trait into scope just to reach `SafeArea::PLUGIN_ID`.
const PLUGIN_ID: &str = "istmo.safe_area";

/// Holds the platform state needed to sample the current safe-area
/// insets from a winit window and publish updates.
///
/// The iOS `UIView` pointer is stored as `usize` (a stable numeric
/// address) rather than `*mut c_void` so the struct auto-derives
/// `Send + Sync` without a workspace-wide `#[allow(unsafe_code)]`.
/// The value is cast back to a pointer inside [`Self::sample`] under a
/// module-local `unsafe` block; winit keeps the underlying UIView
/// alive across the whole event loop.
#[derive(Debug)]
pub struct SafeAreaPublisher {
    runtime: Weak<Runtime>,
    last: Option<SafeAreaInsets>,
    #[cfg(any(target_os = "ios", target_os = "tvos", target_os = "visionos"))]
    ui_view_addr: usize,
    #[cfg(not(any(target_os = "ios", target_os = "tvos", target_os = "visionos")))]
    _phantom: std::marker::PhantomData<()>,
}

impl SafeAreaPublisher {
    /// Install a publisher against a `winit`-owned window.
    ///
    /// # Errors
    /// Returns [`IstmoError::PluginNotDeclared`] when the runtime
    /// enforces declarations and `istmo.safe_area` is missing. A
    /// window-handle-shape mismatch (non-UiKit on iOS, missing handle
    /// altogether) produces an inert publisher whose [`tick`] is a
    /// no-op — the caller stays free of `#[cfg]` at the demo layer,
    /// which is the whole point of the API.
    pub fn install(
        runtime: &Arc<Runtime>,
        window: &impl HasWindowHandle,
    ) -> Result<Self, IstmoError> {
        runtime.check_declared(PLUGIN_ID)?;
        let raw = window.window_handle().ok().map(|h| h.as_raw());
        Ok(Self::from_raw(runtime, raw))
    }

    #[cfg(any(target_os = "ios", target_os = "tvos", target_os = "visionos"))]
    fn from_raw(runtime: &Arc<Runtime>, raw: Option<RawWindowHandle>) -> Self {
        let ui_view_addr = match raw {
            Some(RawWindowHandle::UiKit(h)) => h.ui_view.as_ptr() as usize,
            _ => 0,
        };
        Self {
            runtime: Arc::downgrade(runtime),
            last: None,
            ui_view_addr,
        }
    }

    #[cfg(not(any(target_os = "ios", target_os = "tvos", target_os = "visionos")))]
    fn from_raw(runtime: &Arc<Runtime>, _raw: Option<RawWindowHandle>) -> Self {
        // Non-Apple: publisher is inert. Android's Kotlin publisher
        // still owns the WindowInsets pipeline; a JNI-driven Rust
        // impl lands as a follow-up.
        Self {
            runtime: Arc::downgrade(runtime),
            last: None,
            _phantom: std::marker::PhantomData,
        }
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
            ios::read_insets(self.ui_view_addr)
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
    //! `raw-window-handle` hands us a `UIView` pointer. We message the
    //! view directly for `safeAreaInsets`. Values come in points, which
    //! match Flutter's / egui's logical-pixel model directly.
    //!
    //! `additionalSafeAreaInsets` — the OS updates the effective
    //! `safeAreaInsets` when the keyboard slides up (via the automatic
    //! keyboard layout guide), so a poll-driven reader captures IME
    //! insets without a dedicated `UIKeyboardWillShow` observer. Good
    //! enough for the demo; a dedicated observer would give sub-frame
    //! precision but adds objc2 block lifetimes we do not need yet.

    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    use super::{EdgeInsets, SafeAreaInsets};

    /// # Safety
    /// `view_addr` must be either `0` or the address of a live UIView
    /// owned by winit. Winit keeps the view alive across the whole
    /// event loop, so a stale address means the caller dropped the
    /// window without dropping the publisher first.
    pub fn read_insets(view_addr: usize) -> Option<SafeAreaInsets> {
        if view_addr == 0 {
            return None;
        }
        // SAFETY: `view_addr` is a non-zero address the caller certified
        // points at a live UIView; `msg_send!` with `safeAreaInsets`
        // returns a `UIEdgeInsets` marshalled via the `Encode` impl below.
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
    // to `UIEdgeInsets` in `UIGeometry.h`, so the objc2 marshaller can
    // treat it as the same struct return.
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
