//! Every Apple framework call of the ScreenCaptureKit backend, behind
//! safe wrappers that hand plain Rust data (or opaque, `Send` handles)
//! to the backend logic in `macos/sck.rs`.
//!
//! Threads: ScreenCaptureKit calls back on its own queues; completion
//! handlers post into channels the plugin's threads wait on, with a
//! timeout. Stream outputs run on one serial dispatch queue per stream.
//! Nothing here touches the main thread except the picker, which is
//! presented from the main queue.

#![allow(
    unsafe_code,
    reason = "Objective-C / Core Foundation calls into ScreenCaptureKit, Core Media, Core Video and IOSurface: the FFI this module wraps"
)]

mod audio;
mod buffer;
mod content;
mod picker;
mod stream;

use std::time::Duration;

use objc2::Message;
use objc2::rc::Retained;
use objc2_foundation::{NSError, NSProcessInfo};
use objc2_screen_capture_kit::SCStreamErrorDomain;

use crate::types::{CaptureError, OsVersion};

use super::super::sck_map::{self, ID};

pub(crate) use self::audio::AudioChunk;
pub use self::buffer::IoSurfaceFrame;
pub(crate) use self::buffer::{PixelBuffer, Sample};
pub(crate) use self::content::{
    AppInfo, Content, DisplayInfo, Exclusions, Filter, FilterInfo, FilterStyle, WindowInfo,
};
pub(crate) use self::picker::pick;
pub(crate) use self::stream::{AudioSettings, Kind, Settings, Sink, Stream, screenshot};

/// A retained Objective-C object the plugin moves between its threads.
///
/// SAFETY (for the `Send` / `Sync` impls): only ScreenCaptureKit objects
/// Apple documents as usable from any queue are wrapped: content
/// snapshots and filters (immutable once made), streams (their methods
/// are called from any thread and serialised by the framework), and the
/// plugin's own output / observer objects, whose ivars are `Send + Sync`.
#[derive(Debug)]
struct Shared<T: Message>(Retained<T>);

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "the wrapped objects are thread-safe; see the type documentation"
)]
// SAFETY: see the type documentation.
unsafe impl<T: Message> Send for Shared<T> {}
// SAFETY: see the type documentation.
unsafe impl<T: Message> Sync for Shared<T> {}

impl<T: Message> std::ops::Deref for Shared<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

/// The running macOS version.
#[must_use]
pub(crate) fn os_version() -> OsVersion {
    let version = NSProcessInfo::processInfo().operatingSystemVersion();
    let part = |v: isize| u32::try_from(v).unwrap_or(0);
    OsVersion {
        major: part(version.majorVersion),
        minor: part(version.minorVersion),
        patch: part(version.patchVersion),
    }
}

/// The host time clock (`CMClockGetHostTimeClock`, `mach_absolute_time`)
/// now, in nanoseconds: the clock sample buffers are stamped on.
#[must_use]
pub(crate) fn host_time_ns() -> Option<u64> {
    // SAFETY: `CMClockGetHostTimeClock` returns the process-wide host
    // clock (never null; the binding retains it).
    let clock = unsafe { objc2_core_media::CMClock::host_time_clock() };
    // SAFETY: `CMClockGetTime` only reads a valid clock.
    let now = unsafe { clock.time() };
    sck_map::cmtime_ns(now.value, now.timescale)
}

/// Whether this process holds the Screen Recording grant. Never prompts.
#[must_use]
pub(crate) fn screen_recording_granted() -> bool {
    objc2_core_graphics::CGPreflightScreenCaptureAccess()
}

/// Ask for the Screen Recording grant (the system prompt, at most once
/// per process); whether it is granted now.
#[must_use]
pub(crate) fn request_screen_recording() -> bool {
    objc2_core_graphics::CGRequestScreenCaptureAccess()
}

/// The plugin error for a failed ScreenCaptureKit call.
fn error_of(error: &NSError, operation: &str) -> CaptureError {
    let code = i64::try_from(error.code()).unwrap_or(i64::MIN);
    // SAFETY: `SCStreamErrorDomain` is an immutable framework constant.
    let sck = error
        .domain()
        .isEqualToString(unsafe { SCStreamErrorDomain });
    if sck {
        sck_map::sck_error(operation, code)
    } else {
        tracing::debug!(
            domain = %error.domain(),
            code,
            message = %error.localizedDescription(),
            "screen-capture: {operation} failed"
        );
        CaptureError::os(ID, operation, code)
    }
}

/// Wait for a completion handler's answer.
fn wait<T>(
    rx: &flume::Receiver<Result<T, CaptureError>>,
    timeout: Duration,
    operation: &str,
) -> Result<T, CaptureError> {
    match rx.recv_timeout(timeout) {
        Ok(answer) => answer,
        Err(flume::RecvTimeoutError::Timeout) => Err(CaptureError::protocol(
            ID,
            operation,
            "ScreenCaptureKit did not answer in time",
        )),
        Err(flume::RecvTimeoutError::Disconnected) => Err(CaptureError::protocol(
            ID,
            operation,
            "ScreenCaptureKit dropped the completion handler",
        )),
    }
}
