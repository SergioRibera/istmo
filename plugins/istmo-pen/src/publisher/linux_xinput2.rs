//! X11 XInput2 backend — scaffold.
//!
//! Mirrors the Wayland scaffold: the publisher dispatcher threads the
//! X11 window id + display / xcb-connection pointer through to this
//! module, and the follow-up implementation calls `XISelectEvents` for
//! `XI_Motion` / `XI_ButtonPress` / `XI_ButtonRelease` on tablet-stylus
//! slave pointers, decodes valuator classes for pressure / tilt, and
//! translates into [`PenEvent`] / [`PenHoverEvent`].
//!
//! Two attach entry points because `RawWindowHandle` splits X11 between
//! an Xlib `Display*` and an XCB `xcb_connection_t*`; each delivers the
//! same semantic payload but the follow-up may want to use `x11rb`
//! (XCB) or an Xlib binding depending on the caller. For now both
//! routes record the pointer and return `Ok` so Linux apps using X11
//! windows do not spuriously fail at register time.

#![allow(dead_code)] // scaffold — real backend lands in a follow-up commit

use std::ffi::c_ulong;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;

use super::WindowState;

/// Book-keeping returned from either attach entry point. Carries both
/// potential payloads so the follow-up can disambiguate without
/// re-threading the data through.
#[derive(Debug)]
pub(super) struct XInputAttachment {
    _window: XWindowRef,
    _server: XServerRef,
}

#[derive(Debug)]
enum XWindowRef {
    Xlib(c_ulong),
    Xcb(NonZeroU32),
}

#[derive(Debug)]
enum XServerRef {
    Xlib(NonZeroUsize),
    Xcb(NonZeroUsize),
}

#[derive(Debug)]
pub(super) enum AttachError {
    /// The real backend has not landed yet.
    NotImplemented,
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotImplemented => f.write_str(
                "x11 XInput2 backend is not implemented yet; pending \
                 x11rb integration",
            ),
        }
    }
}

impl std::error::Error for AttachError {}

/// Attach an Xlib `Window` + `Display*` pair.
#[allow(clippy::unnecessary_wraps)]
pub(super) fn attach_xlib(
    window: c_ulong,
    display: NonZeroUsize,
    _state: Arc<WindowState>,
) -> Result<XInputAttachment, AttachError> {
    Ok(XInputAttachment {
        _window: XWindowRef::Xlib(window),
        _server: XServerRef::Xlib(display),
    })
}

/// Attach an XCB `xcb_window_t` + `xcb_connection_t*` pair.
#[allow(clippy::unnecessary_wraps)]
pub(super) fn attach_xcb(
    window: NonZeroU32,
    connection: NonZeroUsize,
    _state: Arc<WindowState>,
) -> Result<XInputAttachment, AttachError> {
    Ok(XInputAttachment {
        _window: XWindowRef::Xcb(window),
        _server: XServerRef::Xcb(connection),
    })
}
