//! The Win32 calls of the backend that have no safe wrapper: the
//! elevation check, the process AUMID and detaching the console.

#![allow(
    unsafe_code,
    reason = "Win32 FFI: process token, explicit AppUserModelID, FreeConsole"
)]

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Console::FreeConsole;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows_core::HSTRING;

/// The process runs elevated (toasts are unsupported there). A failed
/// query counts as not elevated.
pub(super) fn is_elevated() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: no arguments; the pseudo handle needs no closing.
    let process = unsafe { GetCurrentProcess() };
    // SAFETY: `process` is the current process; `token` is a valid out
    // pointer for the call.
    let opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) };
    if opened.is_err() {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut written = 0u32;
    let size = u32::try_from(size_of::<TOKEN_ELEVATION>()).unwrap_or(u32::MAX);
    // SAFETY: `token` was opened with TOKEN_QUERY above; the buffer is a
    // live TOKEN_ELEVATION of `size` bytes and `written` a valid out
    // pointer.
    let queried = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            size,
            &raw mut written,
        )
    };
    // SAFETY: `token` is a handle this function opened and owns.
    let _closed = unsafe { CloseHandle(token) };
    queried.is_ok() && elevation.TokenIsElevated != 0
}

/// Sets the process AUMID (unpackaged apps: taskbar grouping and the
/// toast identity of the process).
pub(super) fn set_process_aumid(aumid: &str) -> windows_core::Result<()> {
    let aumid = HSTRING::from(aumid);
    // SAFETY: `aumid` is a NUL-terminated wide string alive for the call.
    unsafe { SetCurrentProcessExplicitAppUserModelID(&aumid) }
}

/// Detaches from the console COM gave a console-subsystem exe.
pub(super) fn free_console() {
    // SAFETY: no arguments; failing (no console) is harmless.
    let _freed = unsafe { FreeConsole() };
}
