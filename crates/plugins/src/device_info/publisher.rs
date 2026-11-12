//! Pure-Rust `DeviceInfo` sampler.
//!
//! Reads whatever can be resolved without a dep (OS name, arch,
//! CPU core count via `std`; RAM total via `/proc/meminfo` on Linux;
//! kernel release from `/proc` on Linux, Foundation on macOS,
//! `RtlGetVersion` on Windows). Native-side publishers on Android / iOS
//! / macOS / Windows / Web are expected to overwrite this snapshot with
//! richer values (model, manufacturer, screen).

use super::{DeviceInfo, rust_fallback};
use crate::early::LatestPublisher;

/// Publishes the device snapshot the host can resolve.
pub type DeviceInfoPublisher = LatestPublisher<DeviceInfo>;

impl LatestPublisher<DeviceInfo> {
    /// Publish the sampled snapshot. Idempotent — calls after the first
    /// successful publish are no-ops. Call once from app startup after
    /// the runtime is built; native-side bridges remain free to publish
    /// a richer snapshot later.
    pub fn publish(&mut self) {
        if !self.has_published() {
            self.publish_if_changed(enrich(rust_fallback()));
        }
    }
}

fn enrich(mut base: DeviceInfo) -> DeviceInfo {
    #[cfg(target_os = "linux")]
    {
        if let Some(bytes) = linux::ram_total_bytes() {
            base.ram_total_bytes = Some(bytes);
        }
        if let Some(release) = linux::kernel_release() {
            base.os_version = Some(release);
        }
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        if let Some(v) = apple::os_version() {
            base.os_version = Some(v);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(bytes) = windows::ram_total_bytes() {
            base.ram_total_bytes = Some(bytes);
        }
        if let Some(v) = windows::os_version() {
            base.os_version = Some(v);
        }
    }
    base
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;

    pub(super) fn ram_total_bytes() -> Option<u64> {
        let s = fs::read_to_string("/proc/meminfo").ok()?;
        for line in s.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kib: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kib * 1024);
            }
        }
        None
    }

    pub(super) fn kernel_release() -> Option<String> {
        // `/proc/sys/kernel/osrelease` is single-line, portable across
        // Linux distros without needing `uname` FFI.
        fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .map(|s| s.trim().to_owned())
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(unsafe_code)]
mod apple {
    use std::ffi::{CStr, c_char};

    use objc2::msg_send;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};

    pub(super) fn os_version() -> Option<String> {
        // The pool drains the autoreleased NSString before returning, so
        // repeated calls off the main run loop do not accumulate it.
        autoreleasepool(|_| {
            // SAFETY: `NSProcessInfo.processInfo` is a Foundation
            // singleton; `operatingSystemVersionString` returns an
            // autoreleased NSString that lives until this pool drains,
            // and its `UTF8String` buffer is copied before that.
            unsafe {
                let cls = AnyClass::get(c"NSProcessInfo")?;
                let info: *mut AnyObject = msg_send![cls, processInfo];
                if info.is_null() {
                    return None;
                }
                let ns: *mut AnyObject = msg_send![info, operatingSystemVersionString];
                if ns.is_null() {
                    return None;
                }
                let bytes: *const c_char = msg_send![ns, UTF8String];
                if bytes.is_null() {
                    return None;
                }
                CStr::from_ptr(bytes).to_str().ok().map(ToOwned::to_owned)
            }
        })
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows {
    use windows_sys::Wdk::System::SystemServices::RtlGetVersion;
    use windows_sys::Win32::System::SystemInformation::{
        GlobalMemoryStatusEx, MEMORYSTATUSEX, OSVERSIONINFOW,
    };

    pub(super) fn ram_total_bytes() -> Option<u64> {
        let mut info: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        info.dwLength = u32::try_from(std::mem::size_of::<MEMORYSTATUSEX>()).ok()?;
        // SAFETY: `GlobalMemoryStatusEx` fills `info` when `dwLength`
        // matches `size_of::<MEMORYSTATUSEX>()`.
        let ok = unsafe { GlobalMemoryStatusEx(&raw mut info) };
        if ok == 0 {
            return None;
        }
        Some(info.ullTotalPhys)
    }

    pub(super) fn os_version() -> Option<String> {
        // `GetVersionExW` lies on Windows 10/11 processes without a
        // side-by-side compatibility manifest (returns 6.2.9200).
        // `RtlGetVersion` from ntdll always returns the true version.
        let mut info: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
        info.dwOSVersionInfoSize = u32::try_from(std::mem::size_of::<OSVERSIONINFOW>()).ok()?;
        // SAFETY: `RtlGetVersion` writes the version fields on success
        // (returns 0 = STATUS_SUCCESS).
        let status = unsafe { RtlGetVersion(&raw mut info) };
        if status != 0 {
            return None;
        }
        Some(format!(
            "{}.{}.{}",
            info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
        ))
    }
}
