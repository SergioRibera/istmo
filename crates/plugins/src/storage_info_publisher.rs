//! Pure-Rust desktop sampler for [`super::storage_info::StorageInfoClient`].
//!
//! Samples the free + total bytes of the volume holding the current
//! working directory. Linux / macOS use `statvfs` via a hand-rolled
//! `extern "C"` declaration; Windows uses `GetDiskFreeSpaceExW` via
//! `windows-sys`. External volume info is left as `None` on desktop
//! — the semantic (removable SD card / USB stick) does not have a
//! portable equivalent and is better reported by a per-app observer.
//!
//! Idempotent for the current working directory: call [`publish`] on
//! startup and on low-storage warnings or app resume.

use std::sync::{Arc, Weak};

use istmo_core::{IstmoError, Runtime, codec};

use super::storage_info::{STORAGE_INFO_CHANNEL, STORAGE_INFO_PLUGIN_ID, StorageInfo};

#[derive(Debug)]
pub struct StorageInfoPublisher {
    runtime: Weak<Runtime>,
    last: Option<StorageInfo>,
}

impl StorageInfoPublisher {
    pub fn install(runtime: &Arc<Runtime>) -> Result<Self, IstmoError> {
        runtime.check_declared(STORAGE_INFO_PLUGIN_ID)?;
        Ok(Self {
            runtime: Arc::downgrade(runtime),
            last: None,
        })
    }

    /// Sample the primary volume and publish if it changed.
    pub fn tick(&mut self) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let Some(sample) = sample() else {
            return;
        };
        if self.last.as_ref() == Some(&sample) {
            return;
        }
        match codec::encode(&sample) {
            Ok(bytes) => {
                runtime.publish_early_latest(STORAGE_INFO_CHANNEL, bytes);
                self.last = Some(sample);
            }
            Err(err) => tracing::warn!(target = "istmo.storage_info", ?err, "encode failed"),
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
#[allow(unsafe_code, non_camel_case_types)]
mod unix {
    use std::ffi::CString;

    use super::StorageInfo;

    #[repr(C)]
    #[derive(Default)]
    struct Statvfs {
        f_bsize: libc_ulong,
        f_frsize: libc_ulong,
        f_blocks: libc_fsblkcnt,
        f_bfree: libc_fsblkcnt,
        f_bavail: libc_fsblkcnt,
        f_files: libc_fsfilcnt,
        f_ffree: libc_fsfilcnt,
        f_favail: libc_fsfilcnt,
        f_fsid: libc_ulong,
        #[cfg(target_os = "linux")]
        f_flag: libc_ulong,
        #[cfg(target_os = "linux")]
        f_namemax: libc_ulong,
        #[cfg(target_os = "linux")]
        f_type: libc_ulong,
        #[cfg(target_os = "linux")]
        _spare: [libc_ulong; 5],
        #[cfg(target_os = "macos")]
        f_owner: u32,
        #[cfg(target_os = "macos")]
        f_type: u32,
        #[cfg(target_os = "macos")]
        f_flag: libc_ulong,
        #[cfg(target_os = "macos")]
        f_namemax: libc_ulong,
    }

    #[cfg(target_pointer_width = "64")]
    type libc_ulong = u64;
    #[cfg(target_pointer_width = "32")]
    type libc_ulong = u32;
    type libc_fsblkcnt = libc_ulong;
    type libc_fsfilcnt = libc_ulong;

    unsafe extern "C" {
        fn statvfs(path: *const i8, buf: *mut Statvfs) -> i32;
    }

    #[allow(clippy::unnecessary_cast)]
    pub(super) fn sample() -> Option<StorageInfo> {
        let cwd = std::env::current_dir().ok()?;
        let path = CString::new(cwd.into_os_string().into_string().ok()?).ok()?;
        let mut info: Statvfs = Statvfs::default();
        // SAFETY: `path` is a NUL-terminated C string; `info` is a valid
        // `statvfs`-shaped struct for the current platform.
        let ok = unsafe { statvfs(path.as_ptr(), &raw mut info) };
        if ok != 0 {
            return None;
        }
        // Width of `libc_ulong` varies per target (u64 on 64-bit,
        // u32 on 32-bit) — the `as u64` cast is a no-op on 64-bit
        // and widens on 32-bit; clippy's unnecessary-cast lint fires
        // only on the 64-bit path.
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
