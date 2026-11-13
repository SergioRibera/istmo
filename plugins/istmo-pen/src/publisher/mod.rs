//! Desktop-side publisher that adapts a native window to the [`Pen`](crate::Pen)
//! plugin.
//!
//! Multi-window apps hold one [`PenPublisher`] per process and
//! register each `HasWindowHandle` under an app-chosen id — the same id
//! the app passes as [`PenConfig::window_id`](crate::PenConfig::window_id)
//! when constructing its [`PenClient`](crate::PenClient).
//!
//! This module is only compiled on desktop targets (`windows`, `linux`,
//! `macos`). Mobile platforms plug into the plugin through their
//! respective language-side backends under `native/{android,ios}/`.
//!
//! The per-OS backends live in sibling submodules gated on
//! `cfg(target_os = ...)`. On unimplemented OSes the [`PenPublisher`]
//! still keeps its bookkeeping so the client-side surface compiles and
//! runs — events simply never fire until the backend lands.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use flume::{Receiver, Sender, TrySendError};
use istmo_core::Runtime;
use istmo_core::sync::lock;
use istmo_window::{NativeWindow, WindowId, WindowListener, WindowRegistry};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};

use crate::{PenError, PenEvent, PenHoverEvent};

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod linux_wayland;
#[cfg(target_os = "linux")]
mod linux_xinput2;

/// Items each subscriber may have queued before new ones are dropped.
/// A pen reports at up to ~500 Hz, so this is a couple of seconds of
/// samples for a consumer that stopped draining.
pub const SUBSCRIBER_CAPACITY: usize = 1024;

/// One window's stream, fanned out to every subscriber through its own
/// bounded queue.
///
/// A subscriber that falls behind loses the *newest* items once its
/// queue is full (counted in [`Fanout::dropped`]); it never blocks the
/// native event source nor starves other subscribers.
#[derive(Debug)]
struct Fanout<T> {
    subscribers: Mutex<Vec<Sender<T>>>,
    dropped: AtomicU64,
    closed: AtomicBool,
}

impl<T: Clone> Fanout<T> {
    const fn new() -> Self {
        Self {
            subscribers: Mutex::new(Vec::new()),
            dropped: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        }
    }

    /// New subscriber. Already closed when the window is gone.
    fn subscribe(&self) -> Receiver<T> {
        let (tx, rx) = flume::bounded(SUBSCRIBER_CAPACITY);
        let mut subscribers = lock(&self.subscribers);
        if !self.closed.load(Ordering::Acquire) {
            subscribers.push(tx);
        }
        rx
    }

    /// Deliver `item` to every live subscriber, pruning dropped ones.
    /// `true` while at least one subscriber remains.
    fn send(&self, item: &T) -> bool {
        let mut subscribers = lock(&self.subscribers);
        subscribers.retain(|tx| match tx.try_send(item.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        });
        !subscribers.is_empty()
    }

    /// End every subscriber's stream; later subscribers start closed.
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        lock(&self.subscribers).clear();
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Sending half the native backends feed. `send` mirrors
/// [`flume::Sender::send`]'s call shape so backends stay agnostic of the
/// fan-out behind it.
#[derive(Debug)]
struct StreamTx<T>(Fanout<T>);

impl<T: Clone> StreamTx<T> {
    fn send(&self, item: T) -> bool {
        self.0.send(&item)
    }
}

/// Per-registered-window state kept by the publisher.
///
/// [`RawWindowHandle`](raw_window_handle::RawWindowHandle) is not
/// `Send + Sync` (variants carry platform pointers) so we store only the
/// parts each backend actually needs.
#[derive(Debug)]
struct WindowState {
    events_tx: StreamTx<PenEvent>,
    hover_tx: StreamTx<PenHoverEvent>,
    #[cfg(target_os = "windows")]
    windows_attachment: Mutex<Option<windows::WindowAttachment>>,
    #[cfg(target_os = "macos")]
    macos_attachment: Mutex<Option<macos::WindowAttachment>>,
    #[cfg(target_os = "linux")]
    linux_wayland_attachment: Mutex<Option<linux_wayland::WaylandAttachment>>,
    #[cfg(target_os = "linux")]
    linux_xinput2_attachment: Mutex<Option<linux_xinput2::XInputAttachment>>,
}

impl WindowState {
    const fn new() -> Self {
        Self {
            events_tx: StreamTx(Fanout::new()),
            hover_tx: StreamTx(Fanout::new()),
            #[cfg(target_os = "windows")]
            windows_attachment: Mutex::new(None),
            #[cfg(target_os = "macos")]
            macos_attachment: Mutex::new(None),
            #[cfg(target_os = "linux")]
            linux_wayland_attachment: Mutex::new(None),
            #[cfg(target_os = "linux")]
            linux_xinput2_attachment: Mutex::new(None),
        }
    }

    /// Tear down every native hook feeding this window. Linux pump
    /// threads are stopped and joined when their attachment drops.
    fn detach_native(&self) {
        #[cfg(target_os = "windows")]
        {
            let attachment = lock(&self.windows_attachment).take();
            if let Some(attachment) = attachment {
                windows::detach_hwnd(attachment);
            }
        }
        #[cfg(target_os = "macos")]
        {
            let attachment = lock(&self.macos_attachment).take();
            if let Some(attachment) = attachment {
                macos::detach_view(attachment);
            }
        }
        #[cfg(target_os = "linux")]
        {
            drop(lock(&self.linux_wayland_attachment).take());
            drop(lock(&self.linux_xinput2_attachment).take());
        }
    }

    /// End every subscriber's streams and detach.
    ///
    /// Closing comes first so an [`Self::install`] racing this call
    /// either sees the window closed or stores its attachment before
    /// the detach below takes it.
    fn shut_down(&self) {
        self.events_tx.0.close();
        self.hover_tx.0.close();
        self.detach_native();
    }

    /// Put `attachment` in `slot` unless the window was shut down
    /// meanwhile, in which case it is handed back for the caller to
    /// detach. Otherwise an unregister racing an attach would leave a
    /// hook (and, on Linux, a pump thread holding this state) behind.
    #[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux"))]
    fn install<A>(&self, slot: &Mutex<Option<A>>, attachment: A) -> Option<A> {
        let mut slot = lock(slot);
        if self.events_tx.0.closed.load(Ordering::Acquire) {
            return Some(attachment);
        }
        *slot = Some(attachment);
        None
    }
}

/// Process-wide native-input publisher for desktop targets.
///
/// One publisher per app; each open window is registered under a stable
/// integer id that must match the [`PenConfig::window_id`](crate::PenConfig::window_id)
/// used when constructing the matching [`PenClient`](crate::PenClient).
///
/// Windows can be registered directly ([`Self::register_window`]) or
/// through a shared [`WindowRegistry`] the publisher follows via
/// [`Self::follow_registry`], so apps that also use other window-aware
/// plugins register each window only once.
///
/// Dropping the publisher detaches every window and stops the
/// background threads the Linux backends run.
#[derive(Debug)]
pub struct PenPublisher {
    runtime: Arc<Runtime>,
    windows: Mutex<HashMap<u64, Arc<WindowState>>>,
}

impl PenPublisher {
    /// Install the publisher on the given [`Runtime`].
    ///
    /// Each call returns a fresh publisher — the caller keeps the
    /// [`Arc`] and shares it wherever windows are opened. The publisher
    /// holds no process-global state, so multiple installs are safe;
    /// pick one instance and thread it through the app.
    #[must_use]
    pub fn install(runtime: &Arc<Runtime>) -> Arc<Self> {
        Arc::new(Self {
            runtime: Arc::clone(runtime),
            windows: Mutex::new(HashMap::new()),
        })
    }

    /// Register a window under `id`. The id must be the same value the
    /// app then passes as [`PenConfig::window_id`](crate::PenConfig::window_id)
    /// when creating its [`PenClient`](crate::PenClient); mismatched
    /// ids leave the client with no event stream.
    ///
    /// On Windows this installs a `SetWindowSubclass` hook that
    /// intercepts `WM_POINTER*` messages and **must be called on the
    /// thread that created the window**; on macOS it adds an `NSEvent`
    /// monitor; on Linux it starts a Wayland tablet-v2 or `XInput2` pump
    /// thread. Registering an id again replaces (and tears down) the
    /// previous native hook while keeping existing subscribers.
    pub fn register_window<W>(&self, id: u64, window: W) -> Result<(), PenError>
    where
        W: HasWindowHandle + HasDisplayHandle,
    {
        let native =
            NativeWindow::from_window(&window).map_err(|err| PenError::Backend(err.to_string()))?;
        self.attach(id, native)
    }

    /// Subscribe to `registry`: every window registered there (now or
    /// later) is attached under the same id, and unregistering it there
    /// detaches it here.
    pub fn follow_registry(self: &Arc<Self>, registry: &WindowRegistry) -> Result<(), PenError> {
        let listener: Arc<dyn WindowListener> = Arc::clone(self) as Arc<dyn WindowListener>;
        registry
            .add_listener(&listener)
            .map_err(|err| PenError::Backend(err.to_string()))
    }

    #[cfg_attr(
        not(any(target_os = "windows", target_os = "macos", target_os = "linux")),
        allow(unused_variables, clippy::unnecessary_wraps)
    )]
    fn attach(&self, id: u64, native: NativeWindow) -> Result<(), PenError> {
        // Reuse an existing state entry when the id was pre-registered
        // (e.g. via `register_window_id` so a `PenClient` could subscribe
        // before the real window handle was available). Replacing the
        // state would strand those subscribers.
        let state = Arc::clone(
            lock(&self.windows)
                .entry(id)
                .or_insert_with(|| Arc::new(WindowState::new())),
        );
        // A second attach for the same id replaces the first hook.
        state.detach_native();

        #[cfg(target_os = "windows")]
        if let Some(hwnd) = native.hwnd() {
            let attachment = windows::attach_hwnd(hwnd, Arc::clone(&state))
                .map_err(|err| PenError::Backend(err.to_string()))?;
            if let Some(stale) = state.install(&state.windows_attachment, attachment) {
                windows::detach_hwnd(stale);
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(ns_view) = native.ns_view() {
            let attachment = macos::attach_view(ns_view, Arc::clone(&state))
                .map_err(|err| PenError::Backend(err.to_string()))?;
            if let Some(stale) = state.install(&state.macos_attachment, attachment) {
                macos::detach_view(stale);
            }
        }
        #[cfg(target_os = "linux")]
        match native {
            NativeWindow::Wayland { surface, display } => {
                let attachment =
                    linux_wayland::attach_wayland(surface, display, Arc::clone(&state))
                        .map_err(|err| PenError::Backend(err.to_string()))?;
                // A stale attachment stops and joins its pump on drop.
                drop(state.install(&state.linux_wayland_attachment, attachment));
            }
            NativeWindow::Xlib { window, display } => {
                let attachment =
                    linux_xinput2::attach_xlib(window.get(), display, Arc::clone(&state))
                        .map_err(|err| PenError::Backend(err.to_string()))?;
                drop(state.install(&state.linux_xinput2_attachment, attachment));
            }
            NativeWindow::Xcb { window, connection } => {
                let attachment = linux_xinput2::attach_xcb(window, connection, Arc::clone(&state))
                    .map_err(|err| PenError::Backend(err.to_string()))?;
                drop(state.install(&state.linux_xinput2_attachment, attachment));
            }
            // Non-Linux variants or `Detached` land here on Linux
            // builds — the symbolic id still maps, backends simply
            // never fire.
            _ => {}
        }
        Ok(())
    }

    /// Register a window under `id` without binding it to a native
    /// window handle. A no-op when `id` is already registered.
    ///
    /// Suitable for apps that source stylus samples themselves via
    /// [`Self::push_event`] / [`Self::push_hover`] (Wayland tablet-v2,
    /// `XInput2`, egui pointer fallback, unit tests) and for the
    /// `install_libinput` broadcast path — the id must still be present
    /// so the window-state lookup can resolve it, but no `WM_POINTER*` /
    /// `NSEvent` local monitor gets installed. On Windows and macOS the
    /// [`Self::register_window`] variant with a real window handle stays
    /// the recommended path since it wires the OS-level pointer stream
    /// into the plugin.
    pub fn register_window_id(&self, id: u64) {
        lock(&self.windows)
            .entry(id)
            .or_insert_with(|| Arc::new(WindowState::new()));
    }

    /// Unregister a previously registered window: its native hook is
    /// removed, background threads are stopped and joined, and every
    /// subscriber's streams end. Silently ignores ids that were never
    /// registered.
    pub fn unregister_window(&self, id: u64) {
        let removed = lock(&self.windows).remove(&id);
        if let Some(state) = removed {
            state.shut_down();
        }
    }

    /// Subscribe to the [`PenEvent`] stream of a registered window.
    /// Every call returns an independent receiver that sees every
    /// event from then on. `None` if the window was never registered.
    #[must_use]
    pub fn subscribe_events(&self, id: u64) -> Option<Receiver<PenEvent>> {
        Some(self.state_for(id)?.events_tx.0.subscribe())
    }

    /// Subscribe to the [`PenHoverEvent`] stream of a registered
    /// window. Same semantics as [`Self::subscribe_events`].
    #[must_use]
    pub fn subscribe_hover(&self, id: u64) -> Option<Receiver<PenHoverEvent>> {
        Some(self.state_for(id)?.hover_tx.0.subscribe())
    }

    /// Events and hover events dropped for `id` because a subscriber's
    /// queue ([`SUBSCRIBER_CAPACITY`]) was full, counted once per
    /// subscriber that missed one. `None` for unknown ids.
    #[must_use]
    pub fn dropped(&self, id: u64) -> Option<u64> {
        let state = self.state_for(id)?;
        Some(state.events_tx.0.dropped() + state.hover_tx.0.dropped())
    }

    /// Runtime the publisher is bound to.
    #[must_use]
    pub const fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }

    /// Snapshot of the currently registered window ids. Useful for
    /// backends that need to broadcast an event to every listener
    /// (e.g. `libinput` on Linux where samples are seat-scoped rather
    /// than per-window).
    #[must_use]
    pub fn window_ids(&self) -> Vec<u64> {
        lock(&self.windows).keys().copied().collect()
    }

    /// Install the `libinput`-backed sample source on this publisher.
    ///
    /// The backend spawns a single background thread that polls a
    /// process-wide `libinput` context (`seat0` via udev) and
    /// broadcasts every tablet-tool sample to every registered
    /// window. Suitable for kiosk-style apps that own the display;
    /// standard Wayland / X11 clients should route through their
    /// compositor and use [`Self::push_event`] / [`Self::push_hover`]
    /// instead.
    ///
    /// # Errors
    /// Any string returned by the underlying `libinput` setup — a
    /// missing seat, insufficient permissions, or unavailable udev.
    /// The escape hatch [`Self::push_event`] / [`Self::push_hover`]
    /// remains usable regardless: apps can catch the error and fall
    /// back to sourcing samples themselves (e.g. from a compositor
    /// tablet protocol).
    #[cfg(all(target_os = "linux", feature = "libinput_fallback"))]
    pub fn install_libinput(self: &Arc<Self>) -> Result<(), String> {
        linux::install_libinput(self)
    }

    /// Manually publish a [`PenEvent`] to every subscriber of a
    /// registered window. Escape hatch for apps that source stylus
    /// samples from somewhere the built-in backends do not cover —
    /// Wayland tablet-v2, GTK / Qt event pipes, custom compositors, or
    /// unit tests. Returns `true` if the window is registered; `false`
    /// if `id` maps to nothing.
    pub fn push_event(&self, id: u64, event: PenEvent) -> bool {
        let Some(state) = self.state_for(id) else {
            return false;
        };
        let _ = state.events_tx.send(event);
        true
    }

    /// Manually publish a [`PenHoverEvent`]. Symmetric with
    /// [`Self::push_event`].
    pub fn push_hover(&self, id: u64, event: PenHoverEvent) -> bool {
        let Some(state) = self.state_for(id) else {
            return false;
        };
        let _ = state.hover_tx.send(event);
        true
    }

    fn state_for(&self, id: u64) -> Option<Arc<WindowState>> {
        lock(&self.windows).get(&id).cloned()
    }
}

impl Drop for PenPublisher {
    fn drop(&mut self) {
        let windows = std::mem::take(&mut *lock(&self.windows));
        for state in windows.into_values() {
            state.shut_down();
        }
    }
}

impl WindowListener for PenPublisher {
    fn window_registered(&self, id: WindowId, window: NativeWindow) -> Result<(), String> {
        self.attach(id.0, window).map_err(|err| err.to_string())
    }

    fn window_unregistered(&self, id: WindowId) {
        self.unregister_window(id.0);
    }
}
