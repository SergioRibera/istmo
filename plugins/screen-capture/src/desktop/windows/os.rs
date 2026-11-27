//! What the running Windows is: its build (`RtlGetVersion`, which no
//! manifest can make lie), the WinRT API surface present
//! (`ApiInformation`), the QPC clock, the COM apartment the plugin's
//! threads run in, and per-monitor DPI awareness for the calls that
//! report pixels.

#![allow(
    unsafe_code,
    reason = "Win32 FFI: RtlGetVersion, QueryPerformanceCounter, CoIncrementMTAUsage, SetThreadDpiAwarenessContext"
)]

use std::sync::OnceLock;

use windows::Foundation::Metadata::ApiInformation;
use windows::Wdk::System::SystemServices::RtlGetVersion;
use windows::Win32::System::Com::CoIncrementMTAUsage;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::SystemInformation::OSVERSIONINFOW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_core::{HSTRING, s, w};

use crate::types::OsVersion;

/// Windows 10 1903: `IGraphicsCaptureItemInterop` (capture by HWND /
/// HMONITOR) and the free-threaded frame pool.
pub(crate) const BUILD_WGC_INTEROP: u32 = 18_362;
/// Build 20348: `IsBorderRequired`, the borderless consent and process
/// loopback audio.
pub(crate) const BUILD_PROCESS_LOOPBACK: u32 = 20_348;

/// The running OS version (`10.0.<build>` on Windows 10 / 11), read once.
pub(crate) fn version() -> OsVersion {
    static VERSION: OnceLock<OsVersion> = OnceLock::new();
    *VERSION.get_or_init(|| {
        let mut info = OSVERSIONINFOW {
            dwOSVersionInfoSize: u32::try_from(std::mem::size_of::<OSVERSIONINFOW>())
                .unwrap_or(u32::MAX),
            ..OSVERSIONINFOW::default()
        };
        // SAFETY: `info` is a live, writable OSVERSIONINFOW whose size
        // field is set, which is all RtlGetVersion reads.
        let status = unsafe { RtlGetVersion(&raw mut info) };
        if status.is_err() {
            tracing::warn!(status = status.0, "screen-capture: RtlGetVersion failed");
            return OsVersion {
                major: 0,
                minor: 0,
                patch: 0,
            };
        }
        OsVersion {
            major: info.dwMajorVersion,
            minor: info.dwMinorVersion,
            patch: info.dwBuildNumber,
        }
    })
}

/// The build number (`OsVersion::patch` on Windows).
pub(crate) fn build() -> u32 {
    version().patch
}

/// Whether the WinRT runtime class `class` has the property `property`
/// on this machine. A failed query counts as absent.
pub(crate) fn has_property(class: &str, property: &str) -> bool {
    ApiInformation::IsPropertyPresent(&HSTRING::from(class), &HSTRING::from(property))
        .unwrap_or(false)
}

/// Whether the WinRT type `name` exists on this machine.
pub(crate) fn has_type(name: &str) -> bool {
    ApiInformation::IsTypePresent(&HSTRING::from(name)).unwrap_or(false)
}

/// Make sure the process has a multithreaded apartment, so every thread
/// of the plugin (the host's workers, capture threads) that did not
/// initialise COM itself runs in the implicit MTA and may call WinRT and
/// COM. Idempotent; the usage is never released (the plugin may be used
/// until the process exits).
pub(crate) fn ensure_mta() {
    static MTA: OnceLock<bool> = OnceLock::new();
    MTA.get_or_init(|| {
        // SAFETY: no preconditions; the returned cookie is leaked on
        // purpose, which keeps the MTA alive for the process.
        match unsafe { CoIncrementMTAUsage() } {
            Ok(_cookie) => true,
            Err(err) => {
                tracing::warn!(%err, "screen-capture: CoIncrementMTAUsage failed");
                false
            }
        }
    });
}

/// QPC ticks per second, read once.
fn qpc_frequency() -> u64 {
    static FREQUENCY: OnceLock<u64> = OnceLock::new();
    *FREQUENCY.get_or_init(|| {
        let mut frequency = 0_i64;
        // SAFETY: `frequency` is a live, writable i64.
        let read = unsafe { QueryPerformanceFrequency(&raw mut frequency) };
        // Never fails on Windows XP and later; 10 MHz is what Windows 10+
        // reports when it cannot tell otherwise.
        read.ok()
            .and_then(|()| u64::try_from(frequency).ok())
            .filter(|f| *f > 0)
            .unwrap_or(10_000_000)
    })
}

/// A QPC counter value as nanoseconds.
pub(crate) fn qpc_ticks_to_ns(ticks: i64) -> u64 {
    let ticks = u128::try_from(ticks).unwrap_or(0);
    let ns = ticks * 1_000_000_000 / u128::from(qpc_frequency());
    u64::try_from(ns).unwrap_or(u64::MAX)
}

/// `QueryPerformanceCounter` now, in nanoseconds (`PtsClock::Qpc`).
pub(crate) fn qpc_now_ns() -> u64 {
    let mut ticks = 0_i64;
    // SAFETY: `ticks` is a live, writable i64.
    let read = unsafe { QueryPerformanceCounter(&raw mut ticks) };
    if read.is_err() {
        return 0;
    }
    qpc_ticks_to_ns(ticks)
}

/// `SetThreadDpiAwarenessContext`, looked up at run time: it exists from
/// Windows 10 1607 on, and a static import would keep the DLL from
/// loading on 1507 / 1511 (where GDI and DXGI still work).
type SetThreadContext = unsafe extern "system" fn(DPI_AWARENESS_CONTEXT) -> DPI_AWARENESS_CONTEXT;

fn set_thread_context() -> Option<SetThreadContext> {
    static FUNCTION: OnceLock<Option<SetThreadContext>> = OnceLock::new();
    *FUNCTION.get_or_init(|| {
        // SAFETY: user32 is loaded in every GUI-capable process; a missing
        // module answers an error.
        let user32 = unsafe { GetModuleHandleW(w!("user32.dll")) }.ok()?;
        // SAFETY: a live module handle and a NUL-terminated name.
        let address = unsafe { GetProcAddress(user32, s!("SetThreadDpiAwarenessContext")) }?;
        // SAFETY: the export has exactly this signature (winuser.h).
        Some(unsafe {
            std::mem::transmute::<unsafe extern "system" fn() -> isize, SetThreadContext>(address)
        })
    })
}

/// Switch this thread's DPI awareness; `None` where Windows cannot.
fn swap_context(context: DPI_AWARENESS_CONTEXT) -> Option<DPI_AWARENESS_CONTEXT> {
    let set = set_thread_context()?;
    // SAFETY: switching the calling thread's awareness context has no
    // memory-safety preconditions; an unsupported context returns null.
    let previous = unsafe { set(context) };
    (!previous.0.is_null()).then_some(previous)
}

/// Per-monitor DPI awareness (v2) on this thread while the guard lives,
/// so monitor and window rectangles come in physical pixels whatever the
/// process declared in its manifest.
#[derive(Debug)]
pub(crate) struct PhysicalPixels {
    previous: Option<isize>,
}

impl PhysicalPixels {
    pub(crate) fn enter() -> Self {
        Self {
            previous: swap_context(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
                .map(|previous| previous.0 as isize),
        }
    }
}

impl Drop for PhysicalPixels {
    fn drop(&mut self) {
        if let Some(previous) = self.previous {
            // Restores the context this thread had before `enter`.
            let _previous = swap_context(DPI_AWARENESS_CONTEXT(previous as *mut core::ffi::c_void));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qpc_moves_forward() {
        let a = qpc_now_ns();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = qpc_now_ns();
        assert!(b > a, "{a} then {b}");
    }

    #[test]
    fn the_build_is_read() {
        assert_eq!(version().major, 10);
        assert!(build() >= 10_240);
    }
}
