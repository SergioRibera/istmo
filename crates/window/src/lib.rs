//! Process-wide registry of native desktop windows for
//! [`istmo`](https://docs.rs/istmo) plugins.
//!
//! Several desktop backends need the platform window behind an
//! app-chosen id: stylus input subclasses the `HWND` / monitors the
//! `NSWindow`, the share sheet anchors on an `HWND` / `NSView`, and so
//! on. Rather than every plugin asking the app to register its windows
//! again, the app registers each window once in a [`WindowRegistry`]
//! and plugins either look windows up by [`WindowId`] or subscribe as a
//! [`WindowListener`] to react when windows come and go.
//!
//! ```ignore
//! let registry = istmo_window::WindowRegistry::global();
//! registry.register(WindowId(1), &winit_window)?;
//! // …any plugin can now resolve WindowId(1):
//! let hwnd = registry.get(WindowId(1)).and_then(|w| w.hwnd());
//! ```
//!
//! [`NativeWindow`] only stores integers, never borrowed pointers, so
//! it is `Send + Sync + Copy`; converting back to a pointer is the
//! consumer's job and only valid while the app keeps the window alive
//! (i.e. until it calls [`WindowRegistry::unregister`]).

#![doc(html_root_url = "https://docs.rs/istmo-window")]

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fmt;
use std::num::{NonZeroIsize, NonZeroU32, NonZeroUsize};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle};

/// App-chosen identifier for a registered window.
///
/// Plugins that take a window in their configuration (e.g.
/// `PenConfig::window_id`, `ShareRequest::window_id`) carry the raw
/// `u64`; the registry is keyed by the same value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u64);

impl From<u64> for WindowId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl fmt::Display for WindowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "window #{}", self.0)
    }
}

/// Platform window behind a [`WindowId`], stored as plain integers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NativeWindow {
    /// Windows `HWND`.
    Win32 { hwnd: NonZeroIsize },
    /// macOS `NSView*` (the window's content view or a subview).
    AppKit { ns_view: NonZeroUsize },
    /// iOS `UIView*`.
    UiKit { ui_view: NonZeroUsize },
    /// Wayland `wl_surface*`.
    Wayland { surface: NonZeroUsize },
    /// X11 window id (Xlib).
    Xlib { window: std::ffi::c_ulong },
    /// X11 window id (XCB).
    Xcb { window: NonZeroU32 },
    /// Registered without a platform handle — the id exists so lookups
    /// succeed, but backends needing a real window cannot use it.
    Detached,
}

impl NativeWindow {
    /// Resolve the platform window behind `window`.
    pub fn from_window(window: &impl HasWindowHandle) -> Result<Self, WindowError> {
        let handle = window.window_handle().map_err(WindowError::Handle)?;
        Self::try_from(handle.as_raw())
    }

    /// The `HWND` on Windows.
    #[must_use]
    pub const fn hwnd(self) -> Option<NonZeroIsize> {
        match self {
            Self::Win32 { hwnd } => Some(hwnd),
            _ => None,
        }
    }

    /// The `NSView*` on macOS.
    #[must_use]
    pub const fn ns_view(self) -> Option<NonNull<c_void>> {
        match self {
            Self::AppKit { ns_view } => NonNull::new(ns_view.get() as *mut c_void),
            _ => None,
        }
    }

    /// The `UIView*` on iOS.
    #[must_use]
    pub const fn ui_view(self) -> Option<NonNull<c_void>> {
        match self {
            Self::UiKit { ui_view } => NonNull::new(ui_view.get() as *mut c_void),
            _ => None,
        }
    }
}

impl TryFrom<RawWindowHandle> for NativeWindow {
    type Error = WindowError;

    fn try_from(raw: RawWindowHandle) -> Result<Self, Self::Error> {
        let addr = |ptr: NonNull<c_void>| NonZeroUsize::new(ptr.as_ptr() as usize);
        let window = match raw {
            RawWindowHandle::Win32(h) => Some(Self::Win32 { hwnd: h.hwnd }),
            RawWindowHandle::AppKit(h) => addr(h.ns_view).map(|ns_view| Self::AppKit { ns_view }),
            RawWindowHandle::UiKit(h) => addr(h.ui_view).map(|ui_view| Self::UiKit { ui_view }),
            RawWindowHandle::Wayland(h) => addr(h.surface).map(|surface| Self::Wayland { surface }),
            RawWindowHandle::Xlib(h) => Some(Self::Xlib { window: h.window }),
            RawWindowHandle::Xcb(h) => Some(Self::Xcb { window: h.window }),
            _ => None,
        };
        window.ok_or(WindowError::UnsupportedHandle)
    }
}

/// Failure while registering a window.
#[derive(Debug)]
#[non_exhaustive]
pub enum WindowError {
    /// The windowing library could not produce a handle (window not
    /// yet created, already destroyed, …).
    Handle(HandleError),
    /// The handle kind is not one istmo understands.
    UnsupportedHandle,
    /// A [`WindowListener`] refused the window; the registration was
    /// rolled back.
    Listener(String),
}

impl fmt::Display for WindowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Handle(err) => write!(f, "window handle unavailable: {err}"),
            Self::UnsupportedHandle => f.write_str("unsupported raw window handle kind"),
            Self::Listener(msg) => write!(f, "window listener rejected registration: {msg}"),
        }
    }
}

impl std::error::Error for WindowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Handle(err) => Some(err),
            Self::UnsupportedHandle | Self::Listener(_) => None,
        }
    }
}

/// Observer notified when windows are registered or removed.
///
/// Listeners are held weakly: dropping the last `Arc` unsubscribes.
pub trait WindowListener: Send + Sync {
    /// Called for every registration, and once per already-registered
    /// window when the listener is added. Returning an error rolls the
    /// registration back.
    fn window_registered(&self, id: WindowId, window: NativeWindow) -> Result<(), String>;

    /// Called when `id` is unregistered (or rolled back).
    fn window_unregistered(&self, id: WindowId);
}

/// Registry mapping [`WindowId`]s to [`NativeWindow`]s.
#[derive(Default)]
pub struct WindowRegistry {
    windows: Mutex<BTreeMap<WindowId, NativeWindow>>,
    listeners: Mutex<Vec<Weak<dyn WindowListener>>>,
}

impl fmt::Debug for WindowRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowRegistry")
            .field("windows", &*lock(&self.windows))
            .finish_non_exhaustive()
    }
}

impl WindowRegistry {
    /// Process-wide registry shared by every plugin.
    ///
    /// Apps with a single window set use this; tests and embedders that
    /// need isolation build their own with [`Default`].
    #[must_use]
    pub fn global() -> &'static Arc<Self> {
        static GLOBAL: OnceLock<Arc<WindowRegistry>> = OnceLock::new();
        GLOBAL.get_or_init(Arc::default)
    }

    /// Register `window` under `id`, replacing any previous window with
    /// the same id.
    pub fn register(
        &self,
        id: WindowId,
        window: &impl HasWindowHandle,
    ) -> Result<NativeWindow, WindowError> {
        let native = NativeWindow::from_window(window)?;
        self.register_native(id, native)?;
        Ok(native)
    }

    /// Register an already-resolved [`NativeWindow`] (or
    /// [`NativeWindow::Detached`]) under `id`.
    pub fn register_native(&self, id: WindowId, window: NativeWindow) -> Result<(), WindowError> {
        if lock(&self.windows).contains_key(&id) {
            self.unregister(id);
        }
        let listeners = self.live_listeners();
        for (index, listener) in listeners.iter().enumerate() {
            if let Err(msg) = listener.window_registered(id, window) {
                for accepted in &listeners[..index] {
                    accepted.window_unregistered(id);
                }
                return Err(WindowError::Listener(msg));
            }
        }
        lock(&self.windows).insert(id, window);
        Ok(())
    }

    /// Remove `id`. Unknown ids are ignored.
    pub fn unregister(&self, id: WindowId) {
        if lock(&self.windows).remove(&id).is_none() {
            return;
        }
        for listener in self.live_listeners() {
            listener.window_unregistered(id);
        }
    }

    /// Window registered under `id`.
    #[must_use]
    pub fn get(&self, id: WindowId) -> Option<NativeWindow> {
        lock(&self.windows).get(&id).copied()
    }

    /// Every registered id, ascending.
    #[must_use]
    pub fn ids(&self) -> Vec<WindowId> {
        lock(&self.windows).keys().copied().collect()
    }

    /// Any registered window with a platform handle — a fallback anchor
    /// for callers that did not name one. Lowest id wins.
    #[must_use]
    pub fn any_attached(&self) -> Option<(WindowId, NativeWindow)> {
        lock(&self.windows)
            .iter()
            .find(|(_, w)| !matches!(w, NativeWindow::Detached))
            .map(|(id, w)| (*id, *w))
    }

    /// Subscribe `listener` and replay every registered window to it.
    /// Replay errors are returned but do not unregister the window —
    /// other plugins may already depend on it.
    pub fn add_listener(&self, listener: &Arc<dyn WindowListener>) -> Result<(), WindowError> {
        lock(&self.listeners).push(Arc::downgrade(listener));
        let snapshot: Vec<_> = lock(&self.windows)
            .iter()
            .map(|(id, w)| (*id, *w))
            .collect();
        let mut first_error = None;
        for (id, window) in snapshot {
            if let Err(msg) = listener.window_registered(id, window) {
                first_error.get_or_insert(WindowError::Listener(msg));
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn live_listeners(&self) -> Vec<Arc<dyn WindowListener>> {
        let mut listeners = lock(&self.listeners);
        listeners.retain(|weak| weak.strong_count() > 0);
        listeners.iter().filter_map(Weak::upgrade).collect()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Recorder {
        events: Mutex<Vec<String>>,
        reject: Option<u64>,
    }

    impl WindowListener for Recorder {
        fn window_registered(&self, id: WindowId, _window: NativeWindow) -> Result<(), String> {
            if self.reject == Some(id.0) {
                return Err("nope".to_owned());
            }
            lock(&self.events).push(format!("+{}", id.0));
            Ok(())
        }

        fn window_unregistered(&self, id: WindowId) {
            lock(&self.events).push(format!("-{}", id.0));
        }
    }

    #[test]
    fn listeners_see_registrations_and_replays() {
        let registry = WindowRegistry::default();
        registry
            .register_native(WindowId(1), NativeWindow::Detached)
            .expect("register");
        let recorder = Arc::new(Recorder::default());
        let listener: Arc<dyn WindowListener> = recorder.clone();
        registry.add_listener(&listener).expect("add");
        registry
            .register_native(WindowId(2), NativeWindow::Detached)
            .expect("register");
        registry.unregister(WindowId(1));
        assert_eq!(*lock(&recorder.events), ["+1", "+2", "-1"]);
        assert_eq!(registry.ids(), [WindowId(2)]);
    }

    #[test]
    fn rejected_registration_rolls_back() {
        let registry = WindowRegistry::default();
        let ok = Arc::new(Recorder::default());
        let picky = Arc::new(Recorder {
            reject: Some(5),
            ..Recorder::default()
        });
        let ok_listener: Arc<dyn WindowListener> = ok.clone();
        let picky_listener: Arc<dyn WindowListener> = picky;
        registry.add_listener(&ok_listener).expect("add");
        registry.add_listener(&picky_listener).expect("add");
        let err = registry
            .register_native(WindowId(5), NativeWindow::Detached)
            .expect_err("rejected");
        assert!(matches!(err, WindowError::Listener(_)));
        assert!(registry.get(WindowId(5)).is_none());
        assert_eq!(*lock(&ok.events), ["+5", "-5"]);
    }

    #[test]
    fn dropped_listeners_are_pruned() {
        let registry = WindowRegistry::default();
        let listener: Arc<dyn WindowListener> = Arc::new(Recorder::default());
        registry.add_listener(&listener).expect("add");
        drop(listener);
        registry
            .register_native(WindowId(1), NativeWindow::Detached)
            .expect("register");
        assert!(lock(&registry.listeners).is_empty());
    }

    #[test]
    fn any_attached_skips_detached() {
        let registry = WindowRegistry::default();
        registry
            .register_native(WindowId(1), NativeWindow::Detached)
            .expect("register");
        assert!(registry.any_attached().is_none());
        let hwnd = NativeWindow::Win32 {
            hwnd: NonZeroIsize::new(0x10).expect("nonzero"),
        };
        registry
            .register_native(WindowId(2), hwnd)
            .expect("register");
        assert_eq!(registry.any_attached(), Some((WindowId(2), hwnd)));
        assert_eq!(hwnd.hwnd().map(NonZeroIsize::get), Some(0x10));
    }
}
