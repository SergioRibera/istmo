//! Desktop hosts: one platform backend behind the generic [`Host`].

use crate::host::{Host, delegate_tray};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
#[allow(
    unsafe_code,
    reason = "Win32 calls: the notification area, popup menus and GDI bitmaps"
)]
mod windows;

#[cfg(target_os = "linux")]
use linux::start;
#[cfg(target_os = "windows")]
use windows::start;

/// The tray host for Linux, Windows and macOS. Register it with the
/// runtime (`hosts: [TrayClient => DesktopTray::default()]`).
///
/// The platform connection (D-Bus, the notification window, the status
/// bar) is opened on first use, not at registration.
#[derive(Debug)]
pub struct DesktopTray {
    host: Host,
}

impl Default for DesktopTray {
    fn default() -> Self {
        Self {
            host: Host::new(start),
        }
    }
}

delegate_tray!(DesktopTray);
