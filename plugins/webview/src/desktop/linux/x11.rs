#![allow(non_snake_case, reason = "Xlib names kept verbatim")]

//! The few Xlib calls embedding needs, from `libX11.so.6`.

use std::ffi::{c_int, c_uint, c_ulong};

use libloading::Library;

use super::ffi::gpointer;

/// Xlib entry points.
pub(crate) struct X11 {
    pub(crate) XReparentWindow:
        unsafe extern "C" fn(gpointer, c_ulong, c_ulong, c_int, c_int) -> c_int,
    pub(crate) XMoveResizeWindow:
        unsafe extern "C" fn(gpointer, c_ulong, c_int, c_int, c_uint, c_uint) -> c_int,
    pub(crate) XFlush: unsafe extern "C" fn(gpointer) -> c_int,
    _lib: Library,
}

impl X11 {
    pub(crate) fn open() -> Result<Self, String> {
        // SAFETY: the system Xlib, loaded by soname.
        let lib = unsafe { Library::new("libX11.so.6") }.map_err(|e| e.to_string())?;
        // SAFETY: signatures match the Xlib prototypes; `lib` is kept.
        unsafe {
            Ok(Self {
                XReparentWindow: *lib.get(b"XReparentWindow\0").map_err(|e| e.to_string())?,
                XMoveResizeWindow: *lib.get(b"XMoveResizeWindow\0").map_err(|e| e.to_string())?,
                XFlush: *lib.get(b"XFlush\0").map_err(|e| e.to_string())?,
                _lib: lib,
            })
        }
    }
}
