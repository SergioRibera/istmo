//! Time since boot, read from the OS clock on the calling thread.
//!
//! Uptime changes continuously, so it is read on demand instead of being
//! published on an early-event channel: a retained value would be stale
//! the moment it was stored.

/// Milliseconds since the device booted, `None` on targets without a
/// reader.
///
/// - Linux / Android: `clock_gettime(CLOCK_BOOTTIME)`, which keeps
///   counting while the device sleeps.
/// - Apple: `NSProcessInfo.systemUptime`.
/// - Windows: `GetTickCount64`.
#[must_use]
#[allow(
    clippy::unnecessary_wraps,
    clippy::missing_const_for_fn,
    reason = "infallible on Windows only and const where no reader exists; the signature is shared"
)]
pub(super) fn uptime_millis() -> Option<u64> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        boottime()
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos"
    ))]
    {
        apple()
    }
    #[cfg(target_os = "windows")]
    {
        Some(windows())
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos",
        target_os = "windows"
    )))]
    {
        None
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[allow(unsafe_code)]
fn boottime() -> Option<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable `timespec` for the duration of
    // the call and `CLOCK_BOOTTIME` is a clock id the kernel knows.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &raw mut ts) };
    if rc != 0 {
        return None;
    }
    let secs = u64::try_from(ts.tv_sec).ok()?;
    let nanos = u64::try_from(ts.tv_nsec).ok()?;
    secs.checked_mul(1000)?.checked_add(nanos / 1_000_000)
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos"
))]
#[allow(unsafe_code)]
fn apple() -> Option<u64> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    // SAFETY: `NSProcessInfo.processInfo` is a Foundation singleton and
    // `systemUptime` is a thread-safe `NSTimeInterval` getter.
    let seconds: f64 = unsafe {
        let cls = AnyClass::get(c"NSProcessInfo")?;
        let obj: *mut AnyObject = msg_send![cls, processInfo];
        if obj.is_null() {
            return None;
        }
        msg_send![obj, systemUptime]
    };
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "finite, non-negative and far below u64::MAX milliseconds"
    )]
    Some((seconds * 1000.0) as u64)
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
fn windows() -> u64 {
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    // SAFETY: no pointer parameters.
    unsafe { GetTickCount64() }
}
