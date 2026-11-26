//! The Windows source watcher: a hidden top-level window on the watch's
//! own thread receives `WM_DISPLAYCHANGE` (monitors plugged, unplugged,
//! re-moded; broadcasts reach top-level windows only, so not a
//! message-only one), and out-of-context `WinEvent` hooks report windows
//! shown, hidden, destroyed, renamed, moved, minimised, cloaked or
//! focused. Any of them relists after a short settle; the host diffs the
//! listings, so a no-op relisting sends nothing.

#![allow(
    unsafe_code,
    reason = "Win32 FFI: a window class, a message loop and WinEvent hooks"
)]

use std::cell::Cell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE,
    EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_SHOW, EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, MSG, MsgWaitForMultipleObjects,
    OBJID_WINDOW, PM_REMOVE, PeekMessageW, QS_ALLINPUT, RegisterClassExW, TranslateMessage,
    WINDOW_EX_STYLE, WINEVENT_OUTOFCONTEXT, WM_DISPLAYCHANGE, WM_SETTINGCHANGE, WNDCLASSEXW,
    WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows_core::{PCWSTR, w};

use crate::types::{BackendId, CaptureError, SourceList};

use super::super::watch::{SourceFeed, WATCH_POLL};

/// How long one change's events may keep coming before the relisting.
const SETTLE: Duration = Duration::from_millis(50);

const CLASS: PCWSTR = w!("IstmoScreenCaptureWatch");

thread_local! {
    /// Set by the window procedure and the hooks of this thread; the
    /// watch loop on the same thread takes it.
    static DIRTY: Cell<bool> = const { Cell::new(false) };
}

fn mark() {
    DIRTY.with(|d| d.set(true));
}

fn take() -> bool {
    DIRTY.with(Cell::take)
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_DISPLAYCHANGE || msg == WM_SETTINGCHANGE {
        mark();
    }
    // SAFETY: the arguments are the ones Windows passed in.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn on_event(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    // Window-level events only (not carets, cursors, menu items).
    if object == OBJID_WINDOW.0 && child == CHILDID_SELF.cast_signed() && !hwnd.0.is_null() {
        mark();
    }
}

fn register_class(backend: BackendId) -> Result<(), CaptureError> {
    static REGISTERED: OnceLock<Result<(), i64>> = OnceLock::new();
    (*REGISTERED.get_or_init(|| {
        // SAFETY: the module handle of the running executable.
        let instance =
            unsafe { GetModuleHandleW(PCWSTR::null()) }.map_err(|e| i64::from(e.code().0))?;
        let class = WNDCLASSEXW {
            cbSize: u32::try_from(std::mem::size_of::<WNDCLASSEXW>()).unwrap_or(0),
            lpfnWndProc: Some(window_proc),
            hInstance: instance.into(),
            lpszClassName: CLASS,
            ..WNDCLASSEXW::default()
        };
        // SAFETY: `class` is a complete class description with a
        // static name and a valid window procedure.
        let atom = unsafe { RegisterClassExW(&raw const class) };
        if atom == 0 {
            Err(i64::from(windows_core::Error::from_thread().code().0))
        } else {
            Ok(())
        }
    }))
    .map_err(|code| CaptureError::os(backend, "RegisterClassExW", code))
}

/// The hidden window and the hooks, removed on drop (same thread).
struct Listener {
    window: HWND,
    hooks: Vec<HWINEVENTHOOK>,
}

impl Listener {
    fn new(backend: BackendId) -> Result<Self, CaptureError> {
        register_class(backend)?;
        // SAFETY: a registered class; the window is never shown and is
        // destroyed in drop, on this thread.
        let window = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0),
                CLASS,
                w!(""),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            )
        }
        .map_err(|e| CaptureError::os(backend, "CreateWindowExW", i64::from(e.code().0)))?;
        let ranges = [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            (EVENT_OBJECT_DESTROY, EVENT_OBJECT_SHOW + 1),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_NAMECHANGE),
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
        ];
        let hooks = ranges
            .iter()
            .filter_map(|(min, max)| {
                // SAFETY: an out-of-context hook calls `on_event` on this
                // thread's message loop; it is unhooked in drop.
                let hook = unsafe {
                    SetWinEventHook(
                        *min,
                        *max,
                        None,
                        Some(on_event),
                        0,
                        0,
                        WINEVENT_OUTOFCONTEXT,
                    )
                };
                (!hook.is_invalid()).then_some(hook)
            })
            .collect();
        Ok(Self { window, hooks })
    }

    /// Dispatch everything queued; wait up to `timeout` for more first.
    fn pump(timeout: Duration) {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: waits on this thread's message queue only.
        let _woken = unsafe { MsgWaitForMultipleObjects(None, false, ms, QS_ALLINPUT) };
        let mut msg = MSG::default();
        // SAFETY: `msg` is a live out value; messages of every window of
        // this thread (only the listener) are taken.
        while unsafe { PeekMessageW(&raw mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: `msg` came from PeekMessageW.
            let _translated = unsafe { TranslateMessage(&raw const msg) };
            // SAFETY: as above.
            let _result = unsafe { DispatchMessageW(&raw const msg) };
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        for hook in self.hooks.drain(..) {
            // SAFETY: each hook was installed by `new` and is removed once.
            let _unhooked = unsafe { UnhookWinEvent(hook) };
        }
        // SAFETY: the window was created on this thread by `new`.
        let _destroyed = unsafe { DestroyWindow(self.window) };
    }
}

/// Publish `list()` into `feed`, then again after every change, until
/// the subscriber leaves.
pub(crate) fn run(
    backend: BackendId,
    feed: &mut SourceFeed,
    list: impl Fn() -> SourceList,
) -> Result<(), CaptureError> {
    let _listener = Listener::new(backend)?;
    loop {
        let _ = take();
        if !feed.publish(list()) {
            return Ok(());
        }
        loop {
            if feed.is_closed() {
                return Ok(());
            }
            Listener::pump(WATCH_POLL);
            if take() {
                break;
            }
        }
        // One change is often several events (create, show, rename):
        // take them all before listing again.
        let until = Instant::now() + SETTLE;
        while Instant::now() < until {
            Listener::pump(until.saturating_duration_since(Instant::now()));
        }
    }
}
