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

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::fmt;
use std::num::{NonZero, NonZeroIsize, NonZeroU32, NonZeroUsize};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use raw_window_handle::{
    HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
};

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
    /// Wayland `wl_surface*` paired with the compositor's `wl_display*`.
    /// Linux pen / tablet backends need the display to open a secondary
    /// protocol connection; keep both pointers side-by-side so
    /// downstream code never has to recover one from the other.
    Wayland {
        surface: NonZeroUsize,
        display: NonZeroUsize,
    },
    /// X11 window id paired with the Xlib `Display*` the app opened.
    /// The display pointer is required — a window id alone is not
    /// enough to send `XInput2` selects against the server. 0 is X11's
    /// `None` ("no window"), hence the non-zero id.
    Xlib {
        window: NonZero<std::ffi::c_ulong>,
        display: NonZeroUsize,
    },
    /// X11 window id paired with the XCB `xcb_connection_t*`.
    Xcb {
        window: NonZeroU32,
        connection: NonZeroUsize,
    },
    /// Registered without a platform handle — the id exists so lookups
    /// succeed, but backends needing a real window cannot use it.
    Detached,
}

impl NativeWindow {
    /// Resolve the platform window behind `window`. The display handle
    /// is required on Linux (Wayland surface alone is useless without
    /// the matching `wl_display`; `XInput2` selects need the Xlib
    /// `Display*` or an XCB connection). Non-Linux variants ignore the
    /// display payload.
    pub fn from_window<W>(window: &W) -> Result<Self, WindowError>
    where
        W: HasWindowHandle + HasDisplayHandle,
    {
        let window_handle = window.window_handle().map_err(WindowError::Handle)?;
        let display_handle = window.display_handle().map_err(WindowError::Handle)?;
        Self::try_from((window_handle.as_raw(), display_handle.as_raw()))
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

impl TryFrom<(RawWindowHandle, RawDisplayHandle)> for NativeWindow {
    type Error = WindowError;

    fn try_from(
        (window, display): (RawWindowHandle, RawDisplayHandle),
    ) -> Result<Self, Self::Error> {
        let addr = |ptr: NonNull<c_void>| NonZeroUsize::new(ptr.as_ptr() as usize);
        let addr_opt = |ptr: Option<NonNull<c_void>>| ptr.and_then(addr);
        let resolved = match (window, display) {
            (RawWindowHandle::Win32(w), _) => Some(Self::Win32 { hwnd: w.hwnd }),
            (RawWindowHandle::AppKit(w), _) => {
                addr(w.ns_view).map(|ns_view| Self::AppKit { ns_view })
            }
            (RawWindowHandle::UiKit(w), _) => {
                addr(w.ui_view).map(|ui_view| Self::UiKit { ui_view })
            }
            (RawWindowHandle::Wayland(w), RawDisplayHandle::Wayland(d)) => {
                match (addr(w.surface), addr(d.display)) {
                    (Some(surface), Some(display)) => Some(Self::Wayland { surface, display }),
                    _ => None,
                }
            }
            (RawWindowHandle::Xlib(w), RawDisplayHandle::Xlib(d)) => {
                match (NonZero::new(w.window), addr_opt(d.display)) {
                    (Some(window), Some(display)) => Some(Self::Xlib { window, display }),
                    _ => None,
                }
            }
            (RawWindowHandle::Xcb(w), RawDisplayHandle::Xcb(d)) => {
                addr_opt(d.connection).map(|connection| Self::Xcb {
                    window: w.window,
                    connection,
                })
            }
            _ => None,
        };
        resolved.ok_or(WindowError::UnsupportedHandle)
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
    /// A [`WindowListener`] callback called back into a mutating
    /// operation of the registry that is notifying it.
    Reentrant,
}

impl fmt::Display for WindowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Handle(err) => write!(f, "window handle unavailable: {err}"),
            Self::UnsupportedHandle => f.write_str("unsupported raw window handle kind"),
            Self::Listener(msg) => write!(f, "window listener rejected registration: {msg}"),
            Self::Reentrant => {
                f.write_str("window registry mutated from inside one of its listener callbacks")
            }
        }
    }
}

impl std::error::Error for WindowError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Handle(err) => Some(err),
            Self::UnsupportedHandle | Self::Listener(_) | Self::Reentrant => None,
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
///
/// Listener callbacks run on the thread that registers, unregisters or
/// subscribes, outside the window map's lock (so they may call
/// [`WindowRegistry::get`] and the other readers) but inside the
/// registry's operation lock. Calling back into
/// [`WindowRegistry::register`] or [`WindowRegistry::add_listener`] on
/// the same registry from a listener fails with
/// [`WindowError::Reentrant`], and [`WindowRegistry::unregister`] is
/// ignored, instead of deadlocking on that lock.
#[derive(Default)]
pub struct WindowRegistry {
    /// Serialises the mutating operations so the map and every
    /// listener's view of it change together. Without it a window
    /// registered while a listener is being added can reach that
    /// listener twice (live and through the replay), and two
    /// registrations of one id can both miss the other's entry.
    ops: Mutex<()>,
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
    /// the same id. Requires both window and display handle traits —
    /// Linux pen backends read the display pointer; non-Linux ignore it.
    pub fn register<W>(&self, id: WindowId, window: &W) -> Result<NativeWindow, WindowError>
    where
        W: HasWindowHandle + HasDisplayHandle,
    {
        let native = NativeWindow::from_window(window)?;
        self.register_native(id, native)?;
        Ok(native)
    }

    /// Register an already-resolved [`NativeWindow`] (or
    /// [`NativeWindow::Detached`]) under `id`.
    pub fn register_native(&self, id: WindowId, window: NativeWindow) -> Result<(), WindowError> {
        let _ops = self.enter_ops().ok_or(WindowError::Reentrant)?;
        self.unregister_locked(id);
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

    /// Remove `id`. Unknown ids are ignored, and so is a call from inside
    /// one of this registry's listener callbacks.
    pub fn unregister(&self, id: WindowId) {
        let Some(_ops) = self.enter_ops() else {
            return;
        };
        self.unregister_locked(id);
    }

    /// Body of [`Self::unregister`]; the caller holds `ops`.
    fn unregister_locked(&self, id: WindowId) {
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
        let _ops = self.enter_ops().ok_or(WindowError::Reentrant)?;
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

    /// Take the operation lock, or `None` when this thread already holds
    /// it (a listener callback re-entering): `std` mutexes are not
    /// reentrant, so locking again would deadlock.
    fn enter_ops(&self) -> Option<OpsGuard<'_>> {
        let addr = std::ptr::from_ref(self).addr();
        if OPS_HELD.with_borrow(|held| held.contains(&addr)) {
            return None;
        }
        let guard = lock(&self.ops);
        OPS_HELD.with_borrow_mut(|held| held.push(addr));
        Some(OpsGuard { _lock: guard, addr })
    }

    fn live_listeners(&self) -> Vec<Arc<dyn WindowListener>> {
        let mut listeners = lock(&self.listeners);
        listeners.retain(|weak| weak.strong_count() > 0);
        listeners.iter().filter_map(Weak::upgrade).collect()
    }
}

thread_local! {
    /// Addresses of the registries whose operation lock this thread holds.
    static OPS_HELD: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

/// A registry's operation lock plus its [`OPS_HELD`] entry.
struct OpsGuard<'a> {
    _lock: MutexGuard<'a, ()>,
    addr: usize,
}

impl Drop for OpsGuard<'_> {
    fn drop(&mut self) {
        OPS_HELD.with_borrow_mut(|held| {
            if let Some(at) = held.iter().rposition(|&a| a == self.addr) {
                held.swap_remove(at);
            }
        });
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

    /// Re-enters the registry it listens to from every callback.
    struct Reentering {
        registry: Weak<WindowRegistry>,
        seen: Mutex<Vec<String>>,
    }

    impl WindowListener for Reentering {
        fn window_registered(&self, id: WindowId, _window: NativeWindow) -> Result<(), String> {
            let registry = self.registry.upgrade().expect("registry alive");
            let nested = registry.register_native(WindowId(id.0 + 100), NativeWindow::Detached);
            let listener: Arc<dyn WindowListener> = Arc::new(Recorder::default());
            let added = registry.add_listener(&listener);
            registry.unregister(id);
            lock(&self.seen).push(format!(
                "{}/{}/{}",
                matches!(nested, Err(WindowError::Reentrant)),
                matches!(added, Err(WindowError::Reentrant)),
                registry.get(WindowId(id.0 + 100)).is_none(),
            ));
            Ok(())
        }

        fn window_unregistered(&self, _id: WindowId) {}
    }

    #[test]
    fn listener_reentry_fails_instead_of_deadlocking() {
        let registry = Arc::new(WindowRegistry::default());
        let reentering = Arc::new(Reentering {
            registry: Arc::downgrade(&registry),
            seen: Mutex::default(),
        });
        let listener: Arc<dyn WindowListener> = reentering.clone();
        registry.add_listener(&listener).expect("add");
        registry
            .register_native(WindowId(1), NativeWindow::Detached)
            .expect("register");
        assert_eq!(*lock(&reentering.seen), ["true/true/true"]);
        assert_eq!(registry.ids(), [WindowId(1)]);
        // The lock was released: later operations go through.
        registry.unregister(WindowId(1));
        assert!(registry.ids().is_empty());
    }

    #[test]
    fn xlib_window_zero_is_rejected() {
        let display = NonNull::<c_void>::dangling();
        let raw_display =
            RawDisplayHandle::Xlib(raw_window_handle::XlibDisplayHandle::new(Some(display), 0));
        let none = RawWindowHandle::Xlib(raw_window_handle::XlibWindowHandle::new(0));
        assert!(matches!(
            NativeWindow::try_from((none, raw_display)),
            Err(WindowError::UnsupportedHandle)
        ));
        let some = RawWindowHandle::Xlib(raw_window_handle::XlibWindowHandle::new(7));
        let native = NativeWindow::try_from((some, raw_display)).expect("valid xlib window");
        assert!(matches!(native, NativeWindow::Xlib { window, .. } if window.get() == 7));
    }

    #[test]
    fn concurrent_subscribe_never_announces_a_window_twice() {
        for _ in 0..50 {
            let registry = Arc::new(WindowRegistry::default());
            let recorder = Arc::new(Recorder::default());
            let listener: Arc<dyn WindowListener> = recorder.clone();
            let registering = {
                let registry = Arc::clone(&registry);
                std::thread::spawn(move || {
                    for id in 0..20 {
                        registry
                            .register_native(WindowId(id), NativeWindow::Detached)
                            .expect("register");
                    }
                })
            };
            registry.add_listener(&listener).expect("add");
            registering.join().expect("registering thread");
            let mut events = lock(&recorder.events).clone();
            let total = events.len();
            events.sort();
            events.dedup();
            assert_eq!(events.len(), total, "duplicate announcement");
            assert_eq!(total, 20);
        }
    }
}
