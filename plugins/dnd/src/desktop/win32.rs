//! Drags out of Windows windows: OLE drag and drop through the shell.
//!
//! An OLE drag runs a modal loop on the thread that owns the window, the
//! one holding the mouse button. So each window is subclassed when it is
//! attached (on its own thread), and a drag is a message posted to it:
//! the window's thread builds the shell's data object (files by their
//! item ids, text as `CF_UNICODETEXT`) and runs `SHDoDragDrop`, which
//! also draws the drag image and the cursor. The window does not paint
//! while the drag lasts, as with every OLE drag.

use std::mem::ManuallyDrop;
use std::num::NonZeroIsize;
use std::sync::OnceLock;

use windows::Win32::Foundation::{HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{
    CoTaskMemFree, DVASPECT_CONTENT, FORMATETC, IDataObject, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{CF_UNICODETEXT, DROPEFFECT_COPY};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    DefSubclassProc, SHCreateDataObject, SHDoDragDrop, SHParseDisplayName, SetWindowSubclass,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, RegisterWindowMessageW};
use windows::core::{HSTRING, PCWSTR, w};

use super::{DragDone, outgoing};
use crate::{DndError, DragRequest, DragResult};

/// Subclass id of the windows this module handles.
const SUBCLASS: usize = 0x4953_444e; // "ISDN"

/// The message that starts a drag on the window's own thread.
fn drag_message() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    // SAFETY: a constant, null-terminated name.
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(w!("IstmoDndStartDrag")) })
}

/// Subclass `hwnd` so drags can start on its thread. Called on the thread
/// that created the window (where windows get registered).
pub(super) fn attach(hwnd: NonZeroIsize) {
    let window = HWND(hwnd.get() as *mut _);
    let _ = drag_message();
    // SAFETY: a live window of this thread; the procedure outlives it.
    let attached = unsafe { SetWindowSubclass(window, Some(subclass), SUBCLASS, 0) };
    if !attached.as_bool() {
        tracing::warn!(target: "istmo.dnd.windows", "window not subclassed: no drags out of it");
    }
}

/// One drag handed to the window's thread.
struct Pending {
    request: DragRequest,
    done: DragDone,
}

pub(super) fn start_drag(hwnd: NonZeroIsize, request: DragRequest, done: DragDone) {
    let window = HWND(hwnd.get() as *mut _);
    let pending = Box::into_raw(Box::new(Pending { request, done }));
    // SAFETY: the box is reclaimed by the subclass procedure, or here when
    // the message cannot be posted.
    let posted = unsafe {
        PostMessageW(
            Some(window),
            drag_message(),
            WPARAM(0),
            LPARAM(pending as isize),
        )
    };
    if let Err(err) = posted {
        // SAFETY: not posted, so still ours.
        let pending = unsafe { Box::from_raw(pending) };
        pending.done.finish(Err(DndError::Backend(err.to_string())));
    }
}

unsafe extern "system" fn subclass(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if message == drag_message() && lparam.0 != 0 {
        // SAFETY: posted by `start_drag` from `Box::into_raw`, once.
        let pending = unsafe { Box::from_raw(lparam.0 as *mut Pending) };
        let result = drag(window, &pending.request);
        pending.done.finish(result);
        return LRESULT(0);
    }
    // SAFETY: every other message goes on as it came.
    unsafe { DefSubclassProc(window, message, wparam, lparam) }
}

fn backend(err: impl std::fmt::Display) -> DndError {
    DndError::Backend(format!("windows: {err}"))
}

/// Run the drag on the window's thread; returns once it ends.
fn drag(window: HWND, request: &DragRequest) -> Result<DragResult, DndError> {
    // SAFETY: plain state queries.
    let held = unsafe {
        GetAsyncKeyState(i32::from(VK_LBUTTON.0)) < 0
            || GetAsyncKeyState(i32::from(VK_RBUTTON.0)) < 0
    };
    if !held {
        return Err(DndError::NotPressed);
    }
    let data = data_object(request)?;
    // SAFETY: `data` is a valid data object; no drop source means the
    // shell's own, with its drag image.
    match unsafe { SHDoDragDrop(Some(window), &data, None, DROPEFFECT_COPY) } {
        Ok(effect) if effect != windows::Win32::System::Ole::DROPEFFECT_NONE => {
            Ok(DragResult::Dropped)
        }
        Ok(_) => Ok(DragResult::Cancelled),
        Err(err) => Err(backend(err)),
    }
}

/// The shell's data object for the drag's files, with its text added.
fn data_object(request: &DragRequest) -> Result<IDataObject, DndError> {
    let mut ids: Vec<*mut ITEMIDLIST> = Vec::new();
    for path in outgoing::files(&request.items) {
        let wide = HSTRING::from(path.as_os_str());
        let mut id: *mut ITEMIDLIST = std::ptr::null_mut();
        // SAFETY: `wide` lives across the call; `id` is freed below.
        unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &raw mut id, 0, None) }
            .map_err(backend)?;
        ids.push(id);
    }
    let children: Vec<*const ITEMIDLIST> = ids.iter().map(|id| id.cast_const()).collect();
    // SAFETY: absolute item ids, relative to the desktop (no folder).
    let data: Result<IDataObject, _> = unsafe {
        SHCreateDataObject(
            None,
            (!children.is_empty()).then_some(children.as_slice()),
            None,
        )
    };
    for id in ids {
        // SAFETY: allocated by `SHParseDisplayName`.
        unsafe { CoTaskMemFree(Some(id.cast_const().cast())) };
    }
    let data = data.map_err(backend)?;
    if let Some(text) = outgoing::plain_text(&request.items) {
        set_text(&data, &text)?;
    }
    Ok(data)
}

/// Add `text` to `data` as `CF_UNICODETEXT`.
fn set_text(data: &IDataObject, text: &str) -> Result<(), DndError> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * 2;
    // SAFETY: a fresh movable block, filled while locked; handed to the
    // data object, which frees it (`release` true).
    unsafe {
        let global: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes).map_err(backend)?;
        let target = GlobalLock(global).cast::<u16>();
        if target.is_null() {
            return Err(backend("cannot lock the text block"));
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
        let _ = GlobalUnlock(global);
        let format = FORMATETC {
            cfFormat: CF_UNICODETEXT.0,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        let medium = STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: global },
            pUnkForRelease: ManuallyDrop::new(None),
        };
        data.SetData(&raw const format, &raw const medium, true)
            .map_err(backend)
    }
}
