//! Pure-Rust desktop sampler for
//! [`StorageInfoClient`](super::storage_info::StorageInfoClient).
//!
//! Samples the free + total bytes of the volume holding the current
//! working directory. Linux / macOS use `libc::statvfs` so the struct
//! layout is correct per platform (Darwin `fsblkcnt_t` = `uint32_t`,
//! Linux = `u_long`); Windows uses `GetDiskFreeSpaceExW` via
//! `windows-sys`. External volume info is left as `None` on desktop
//! — the semantic (removable SD card / USB stick) does not have a
//! portable equivalent and is better reported by a per-app observer.
//!
//! Call [`StorageInfoPublisher::tick`] on startup and on low-storage
//! warnings or app resume; it publishes only when the numbers change.

use super::storage_info::StorageInfo;
use crate::early::LatestPublisher;

/// Publishes the free and total space of the working directory volume.
pub type StorageInfoPublisher = LatestPublisher<StorageInfo>;

impl LatestPublisher<StorageInfo> {
    /// Sample the primary volume and publish if it changed.
    pub fn tick(&mut self) {
        if let Some(sample) = sample() {
            self.publish_if_changed(sample);
        }
    }
}

fn sample() -> Option<StorageInfo> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        unix::sample()
    }
    #[cfg(target_os = "windows")]
    {
        windows::sample()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(unsafe_code)]
mod unix {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStringExt;

    use super::StorageInfo;

    #[allow(clippy::unnecessary_cast)]
    pub(super) fn sample() -> Option<StorageInfo> {
        let cwd = std::env::current_dir().ok()?;
        let path = CString::new(cwd.into_os_string().into_vec()).ok()?;
        // SAFETY: `path` is a NUL-terminated C string; `info` is a
        // platform-correct `libc::statvfs` struct (Darwin uses u32
        // block counts, Linux uses u_long — libc gets the layout
        // right per cfg).
        let mut info: libc::statvfs = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::statvfs(path.as_ptr(), &raw mut info) };
        if ok != 0 {
            return None;
        }
        // Cast through u64 explicitly — field widths differ across
        // targets (macOS u32, 64-bit Linux u64); the cast is a no-op
        // on 64-bit and widens on 32-bit.
        let block = info.f_frsize as u64;
        Some(StorageInfo {
            primary_free_bytes: info.f_bavail as u64 * block,
            primary_total_bytes: info.f_blocks as u64 * block,
            external: None,
        })
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    use super::StorageInfo;

    pub(super) fn sample() -> Option<StorageInfo> {
        let cwd = std::env::current_dir().ok()?;
        let mut wide: Vec<u16> = cwd.as_os_str().encode_wide().collect();
        wide.push(0);
        let mut free_available: u64 = 0;
        let mut total: u64 = 0;
        let mut free_total: u64 = 0;
        // SAFETY: `wide` is NUL-terminated UTF-16; the three out-params
        // are valid u64 slots.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &raw mut free_available,
                &raw mut total,
                &raw mut free_total,
            )
        };
        if ok == 0 {
            return None;
        }
        Some(StorageInfo {
            primary_free_bytes: free_available,
            primary_total_bytes: total,
            external: None,
        })
    }
}
