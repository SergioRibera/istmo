//! Pure-Rust `DeviceInfo` sampler.
//!
//! Reads whatever can be resolved without a dep (OS name, arch,
//! CPU core count via `std`; RAM total via `/proc/meminfo` on Linux;
//! kernel release via `uname` on unix). Native-side publishers on
//! Android / iOS / macOS / Windows / Web are expected to overwrite
//! this snapshot with richer values (model, manufacturer, screen).

use std::sync::{Arc, Weak};

use istmo_core::{IstmoError, Runtime, codec};

use super::{DEVICE_INFO_CHANNEL, DEVICE_INFO_PLUGIN_ID, DeviceInfo, rust_fallback};

#[derive(Debug)]
pub struct DeviceInfoPublisher {
    runtime: Weak<Runtime>,
    emitted: bool,
}

impl DeviceInfoPublisher {
    pub fn install(runtime: &Arc<Runtime>) -> Result<Self, IstmoError> {
        runtime.check_declared(DEVICE_INFO_PLUGIN_ID)?;
        Ok(Self {
            runtime: Arc::downgrade(runtime),
            emitted: false,
        })
    }

    /// Publish the sampled snapshot. Idempotent — subsequent calls
    /// are no-ops. Call once from app startup after the runtime is
    /// built; native-side bridges remain free to publish a richer
    /// snapshot later.
    pub fn publish(&mut self) {
        if self.emitted {
            return;
        }
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let snapshot = enrich(rust_fallback());
        match codec::encode(&snapshot) {
            Ok(bytes) => {
                runtime.publish_early_latest(DEVICE_INFO_CHANNEL, bytes);
                self.emitted = true;
            }
            Err(err) => tracing::warn!(target = "istmo.device_info", ?err, "encode failed"),
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
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    pub(super) fn os_version() -> Option<String> {
        // SAFETY: `NSProcessInfo.processInfo` is a Foundation singleton
        // and `operatingSystemVersionString` returns an NSString valid
        // for the autorelease pool scope — copied immediately.
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
            let bytes: *const i8 = msg_send![ns, UTF8String];
            if bytes.is_null() {
                return None;
            }
            let c_str = core::ffi::CStr::from_ptr(bytes);
            c_str.to_str().ok().map(ToOwned::to_owned)
        }
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows {
    use windows_sys::Win32::System::SystemInformation::{
        GetVersionExW, GlobalMemoryStatusEx, MEMORYSTATUSEX, OSVERSIONINFOW,
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
        let mut info: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
        info.dwOSVersionInfoSize = u32::try_from(std::mem::size_of::<OSVERSIONINFOW>()).ok()?;
        // SAFETY: `GetVersionExW` is deprecated but still writes the
        // version fields when passed a correctly-sized struct.
        let ok = unsafe { GetVersionExW(&raw mut info) };
        if ok == 0 {
            return None;
        }
        Some(format!(
            "{}.{}.{}",
            info.dwMajorVersion, info.dwMinorVersion, info.dwBuildNumber
        ))
    }
}
