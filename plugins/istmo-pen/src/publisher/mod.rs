//! Desktop-side publisher that adapts a native window to the [`Pen`]
//! plugin. Multi-window apps hold one [`PenPublisher`] per process and
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
use std::sync::{Arc, Mutex};

use flume::{Receiver, Sender};
use istmo_core::Runtime;
use raw_window_handle::{HandleError, HasWindowHandle};
#[cfg(target_os = "windows")]
use raw_window_handle::RawWindowHandle;

use crate::{PenError, PenEvent, PenHoverEvent};

#[cfg(target_os = "windows")]
mod windows;

/// Per-registered-window state kept by the publisher.
///
/// [`RawWindowHandle`] is not `Send + Sync` (variants carry platform
/// pointers) so we store only the parts each backend actually needs —
/// the extracted `HWND` on Windows, nothing on other targets until
/// their backends land.
#[derive(Debug)]
struct WindowState {
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    events_tx: Sender<PenEvent>,
    events_rx: Mutex<Option<Receiver<PenEvent>>>,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    hover_tx: Sender<PenHoverEvent>,
    hover_rx: Mutex<Option<Receiver<PenHoverEvent>>>,
    #[cfg(target_os = "windows")]
    windows_attachment: Mutex<Option<windows::WindowAttachment>>,
}

impl WindowState {
    fn new() -> Self {
        let (events_tx, events_rx) = flume::unbounded();
        let (hover_tx, hover_rx) = flume::unbounded();
        Self {
            events_tx,
            events_rx: Mutex::new(Some(events_rx)),
            hover_tx,
            hover_rx: Mutex::new(Some(hover_rx)),
            #[cfg(target_os = "windows")]
            windows_attachment: Mutex::new(None),
        }
    }
}

/// Process-wide native-input publisher for desktop targets.
///
/// One publisher per app; each open window is registered under a stable
/// integer id that must match the [`PenConfig::window_id`](crate::PenConfig::window_id)
/// used when constructing the matching [`PenClient`](crate::PenClient).
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
    /// intercepts `WM_POINTER*` messages; on other desktop OSes it
    /// currently only records the mapping (backend lands in a later
    /// milestone). Fails only when the raw handle cannot be resolved.
    pub fn register_window(
        &self,
        id: u64,
        window: impl HasWindowHandle,
    ) -> Result<(), PenError> {
        let handle = window
            .window_handle()
            .map_err(|err: HandleError| PenError::Backend(err.to_string()))?;
        let state = Arc::new(WindowState::new());

        #[cfg(target_os = "windows")]
        if let RawWindowHandle::Win32(win32) = handle.as_raw() {
            let attachment = windows::attach_hwnd(win32.hwnd, Arc::clone(&state))
                .map_err(|err| PenError::Backend(err.to_string()))?;
            match state.windows_attachment.lock() {
                Ok(mut guard) => *guard = Some(attachment),
                Err(poisoned) => *poisoned.into_inner() = Some(attachment),
            }
        }
        #[cfg(not(target_os = "windows"))]
        let _ = handle;

        let mut guard = match self.windows.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.insert(id, state);
        Ok(())
    }

    /// Unregister a previously registered window. Silently ignores ids
    /// that were never registered.
    pub fn unregister_window(&self, id: u64) {
        let removed = {
            let mut guard = match self.windows.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.remove(&id)
        };
        if let Some(_state) = removed {
            #[cfg(target_os = "windows")]
            {
                let attachment = match _state.windows_attachment.lock() {
                    Ok(mut guard) => guard.take(),
                    Err(poisoned) => poisoned.into_inner().take(),
                };
                if let Some(attachment) = attachment {
                    windows::detach_hwnd(attachment);
                }
            }
        }
    }

    /// Take ownership of the [`PenEvent`] stream for a registered
    /// window. Returns `None` if the window was never registered or if
    /// the events receiver was already handed out.
    #[must_use]
    pub fn subscribe_events(&self, id: u64) -> Option<Receiver<PenEvent>> {
        let state = self.state_for(id)?;
        let mut slot = match state.events_rx.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        slot.take()
    }

    /// Take ownership of the [`PenHoverEvent`] stream for a registered
    /// window. Same semantics as [`Self::subscribe_events`].
    #[must_use]
    pub fn subscribe_hover(&self, id: u64) -> Option<Receiver<PenHoverEvent>> {
        let state = self.state_for(id)?;
        let mut slot = match state.hover_rx.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        slot.take()
    }

    /// Runtime the publisher is bound to.
    #[must_use]
    pub fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }

    fn state_for(&self, id: u64) -> Option<Arc<WindowState>> {
        let guard = match self.windows.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.get(&id).cloned()
    }
}
