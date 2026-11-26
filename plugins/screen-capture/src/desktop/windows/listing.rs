//! Monitors and windows as `Source`s, shared by every Windows backend.
//!
//! - Monitors: `EnumDisplayMonitors` + `GetMonitorInfoW` (geometry,
//!   primary, GDI device name), `EnumDisplaySettingsW` (refresh,
//!   orientation), `EnumDisplayDevicesW` (the platform's description)
//!   and the DisplayConfig target name (EDID make / model, connector).
//! - Windows: `EnumWindows`, kept to what a person sees as an app window
//!   (visible, not cloaked, not a tool window, unowned or an app
//!   window, titled, non-empty); title, executable name (the app id) and
//!   pid. Titles are sensitive: the host lists sources only to callers
//!   in this process unless the app opens `sources`.
//!
//! Geometry is in physical desktop pixels (the virtual-screen space a
//! per-monitor DPI-aware Windows app sees), so `scale_permille` is 1000:
//! Windows places monitors of different DPI side by side in pixels, and
//! dividing each by its own scale would make them overlap.
//!
//! Ids: `monitor:<GDI device name>` (`monitor:\\.\DISPLAY1`, stable while
//! the display configuration is) and `window:<HWND in hex>`.

#![allow(
    unsafe_code,
    reason = "Win32 FFI: EnumDisplayMonitors, EnumWindows, DisplayConfig, DWM attributes"
)]

use std::ffi::c_void;

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
    DISPLAYCONFIG_DEVICE_INFO_HEADER, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EXTERNAL,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_USB_TUNNEL, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DVI,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HD15, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_VIRTUAL,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_WIRED, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_MIRACAST,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, DISPLAY_DEVICEW, DMDO_90, DMDO_180, DMDO_270, ENUM_CURRENT_SETTINGS,
    EnumDisplayDevicesW, EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, HDC, HMONITOR,
    MONITOR_DEFAULTTONULL, MONITORINFO, MONITORINFOEXW, MonitorFromPoint,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetCursorPos, GetForegroundWindow, GetWindow,
    GetWindowDisplayAffinity, GetWindowLongW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindow, IsWindowVisible, MONITORINFOF_PRIMARY, SMTO_ABORTIFHUNG, SMTO_BLOCK,
    SendMessageTimeoutW, WM_GETTEXT, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};
use windows_core::{BOOL, PCWSTR, PWSTR};

use crate::types::{
    BackendId, CaptureError, Orientation, Rect, Requirement, Rotation, Size, Source, SourceCapture,
    SourceId, SourceKind,
};

use super::os::PhysicalPixels;
use super::wide_to_string;

/// What a source id names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// A monitor by GDI device name.
    Monitor(String),
    Window(isize),
}

/// Parse a Windows source id value.
pub(crate) fn parse(value: &str) -> Option<Target> {
    if let Some(device) = value.strip_prefix("monitor:") {
        return (!device.is_empty()).then(|| Target::Monitor(device.to_owned()));
    }
    let hex = value.strip_prefix("window:")?;
    isize::from_str_radix(hex, 16)
        .ok()
        .filter(|h| *h != 0)
        .map(Target::Window)
}

fn window_id(hwnd: isize) -> String {
    format!("window:{hwnd:x}")
}

/// The target of `source`, for `backend`.
pub(crate) fn target_of(backend: BackendId, source: &Source) -> Result<Target, CaptureError> {
    parse(&source.id.value)
        .ok_or_else(|| CaptureError::SourceNotFound(source.id.clone()))
        .and_then(|target| {
            if source.id.backend == backend {
                Ok(target)
            } else {
                Err(CaptureError::SourceNotFound(source.id.clone()))
            }
        })
}

const fn rect_of(r: RECT) -> Rect {
    Rect::new(
        r.left,
        r.top,
        r.right.saturating_sub(r.left).unsigned_abs(),
        r.bottom.saturating_sub(r.top).unsigned_abs(),
    )
}

/// A monitor as the backends need it.
#[derive(Debug, Clone)]
pub(crate) struct Monitor {
    pub(crate) hmonitor: isize,
    /// GDI device name, `\\.\DISPLAY1`.
    pub(crate) device: String,
    /// Desktop rectangle, physical pixels.
    pub(crate) rect: Rect,
    pub(crate) source: Source,
}

impl Monitor {
    pub(crate) const fn handle(&self) -> HMONITOR {
        HMONITOR(self.hmonitor as *mut c_void)
    }
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the `&mut Vec<isize>` `monitor_handles` passed
    // in, alive for the whole (synchronous) enumeration.
    let handles = unsafe { &mut *(data.0 as *mut Vec<isize>) };
    handles.push(monitor.0 as isize);
    BOOL(1)
}

fn monitor_handles() -> Vec<isize> {
    let mut handles: Vec<isize> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, which outlives the
    // synchronous enumeration.
    let _all = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM((&raw mut handles) as isize),
        )
    };
    handles
}

/// Make, model and connector of the monitor on GDI device `device`, from
/// the active DisplayConfig paths.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct TargetName {
    make: Option<String>,
    model: Option<String>,
    connector: Option<String>,
}

/// The EDID manufacturer id as DisplayConfig reports it (the EDID's
/// big-endian bytes read as a little-endian `u16`) → its three letters.
fn pnp_id(raw: u16) -> Option<String> {
    let id = raw.swap_bytes();
    let letters: Option<String> = [10_u16, 5, 0]
        .into_iter()
        .map(|shift| {
            let code = u8::try_from((id >> shift) & 0x1f).ok()?;
            (1..=26)
                .contains(&code)
                .then(|| char::from(b'A' + code - 1))
        })
        .collect();
    letters
}

fn connector_name(
    technology: DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY,
    instance: u32,
) -> Option<String> {
    let kind = match technology {
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI => "HDMI",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EXTERNAL => "DP",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED => "eDP",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_USB_TUNNEL => "DP-USB",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DVI => "DVI",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HD15 => "VGA",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS => "LVDS",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL => "Internal",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_MIRACAST => "Miracast",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_WIRED => "Indirect",
        DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_VIRTUAL => "Virtual",
        _ => return None,
    };
    Some(format!("{kind}-{}", instance.saturating_add(1)))
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim().to_owned();
    (!s.is_empty()).then_some(s)
}

/// Target names per GDI device name, for every active path.
fn target_names() -> Vec<(String, TargetName)> {
    let mut paths_len = 0_u32;
    let mut modes_len = 0_u32;
    // SAFETY: both out pointers are live locals.
    let sized = unsafe {
        GetDisplayConfigBufferSizes(
            QDC_ONLY_ACTIVE_PATHS,
            &raw mut paths_len,
            &raw mut modes_len,
        )
    };
    if sized != ERROR_SUCCESS {
        return Vec::new();
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); paths_len as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); modes_len as usize];
    // SAFETY: the arrays hold `paths_len` / `modes_len` elements, the
    // counts QueryDisplayConfig may write and then updates.
    let queried = unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &raw mut paths_len,
            paths.as_mut_ptr(),
            &raw mut modes_len,
            modes.as_mut_ptr(),
            None,
        )
    };
    if queried != ERROR_SUCCESS {
        return Vec::new();
    }
    paths.truncate(paths_len as usize);
    paths
        .iter()
        .filter_map(|path| {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: u32::try_from(std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>())
                        .unwrap_or(0),
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                ..DISPLAYCONFIG_SOURCE_DEVICE_NAME::default()
            };
            // SAFETY: `source` is a live packet whose header names its own
            // type and size.
            let status = unsafe { DisplayConfigGetDeviceInfo(&raw mut source.header) };
            if status != 0 {
                return None;
            }
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    size: u32::try_from(std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>())
                        .unwrap_or(0),
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                },
                ..DISPLAYCONFIG_TARGET_DEVICE_NAME::default()
            };
            // SAFETY: as above, for the target-name packet.
            let status = unsafe { DisplayConfigGetDeviceInfo(&raw mut target.header) };
            if status != 0 {
                return None;
            }
            // SAFETY: the flags union is a plain `u32` bit field.
            let flags = unsafe { target.flags.Anonymous.value };
            let edid_ids = flags & 0x2 != 0;
            Some((
                wide_to_string(&source.viewGdiDeviceName),
                TargetName {
                    make: edid_ids.then(|| pnp_id(target.edidManufactureId)).flatten(),
                    model: non_empty(&wide_to_string(&target.monitorFriendlyDeviceName)),
                    connector: connector_name(target.outputTechnology, target.connectorInstance),
                },
            ))
        })
        .collect()
}

/// `dmDisplayOrientation` → what a buffer in scan-out orientation needs
/// to be shown upright (DXGI duplication delivers that orientation).
const fn scanout_orientation(rotation: i32) -> Orientation {
    let rotation = match rotation {
        r if r == DMDO_90.0.cast_signed() => Rotation::R90,
        r if r == DMDO_180.0.cast_signed() => Rotation::R180,
        r if r == DMDO_270.0.cast_signed() => Rotation::R270,
        _ => Rotation::R0,
    };
    Orientation {
        mirrored: false,
        rotation,
    }
}

fn describe_monitor(
    backend: BackendId,
    hmonitor: isize,
    names: &[(String, TargetName)],
    scanout: bool,
) -> Option<Monitor> {
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: u32::try_from(std::mem::size_of::<MONITORINFOEXW>()).unwrap_or(0),
            ..MONITORINFO::default()
        },
        ..MONITORINFOEXW::default()
    };
    let handle = HMONITOR(hmonitor as *mut c_void);
    // SAFETY: `info` is a live MONITORINFOEXW whose cbSize says so.
    let ok = unsafe { GetMonitorInfoW(handle, (&raw mut info).cast()) };
    if !ok.as_bool() {
        return None;
    }
    let device = wide_to_string(&info.szDevice);
    let rect = rect_of(info.monitorInfo.rcMonitor);
    let mut mode = DEVMODEW {
        dmSize: u16::try_from(std::mem::size_of::<DEVMODEW>()).unwrap_or(0),
        ..DEVMODEW::default()
    };
    let device_w: Vec<u16> = device.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `device_w` is NUL-terminated and `mode` is a live DEVMODEW
    // whose dmSize says so.
    let have_mode = unsafe {
        EnumDisplaySettingsW(
            PCWSTR(device_w.as_ptr()),
            ENUM_CURRENT_SETTINGS,
            &raw mut mode,
        )
    }
    .as_bool();
    let mut adapter_device = DISPLAY_DEVICEW {
        cb: u32::try_from(std::mem::size_of::<DISPLAY_DEVICEW>()).unwrap_or(0),
        ..DISPLAY_DEVICEW::default()
    };
    // SAFETY: as above; device 0 of a display adapter is its monitor.
    let have_device =
        unsafe { EnumDisplayDevicesW(PCWSTR(device_w.as_ptr()), 0, &raw mut adapter_device, 0) }
            .as_bool();
    let name = names
        .iter()
        .find(|(d, _)| *d == device)
        .map(|(_, n)| n.clone())
        .unwrap_or_default();
    let mut source = Source::new(
        SourceId::new(backend, format!("monitor:{device}")),
        SourceKind::Monitor,
        name.model
            .clone()
            .unwrap_or_else(|| device.trim_start_matches(r"\\.\").to_owned()),
    );
    source.geometry = Some(rect);
    source.physical_size = Some(Size::new(rect.width, rect.height));
    source.scale_permille = Some(1000);
    if have_mode {
        source.refresh_mhz = (mode.dmDisplayFrequency > 1).then(|| mode.dmDisplayFrequency * 1000);
        if scanout {
            // SAFETY: for a display device, the display arm of the union
            // is the one EnumDisplaySettingsW fills.
            let rotation = unsafe { mode.Anonymous1.Anonymous2.dmDisplayOrientation };
            source.orientation = scanout_orientation(rotation.0.cast_signed());
        }
    }
    source.flags.primary = info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0;
    source.make = name.make;
    source.model = name.model;
    source.connector = name.connector;
    source.description = have_device
        .then(|| non_empty(&wide_to_string(&adapter_device.DeviceString)))
        .flatten();
    Some(Monitor {
        hmonitor,
        device,
        rect,
        source,
    })
}

/// Every monitor, primary first, then by position. `scanout`: the
/// backend delivers buffers in scan-out orientation (DXGI), so
/// `Source::orientation` says how to turn them.
pub(crate) fn monitors(backend: BackendId, scanout: bool) -> Vec<Monitor> {
    let _pixels = PhysicalPixels::enter();
    let names = target_names();
    let mut monitors: Vec<Monitor> = monitor_handles()
        .into_iter()
        .filter_map(|h| describe_monitor(backend, h, &names, scanout))
        .collect();
    monitors.sort_by_key(|m| (!m.source.flags.primary, m.rect.y, m.rect.x));
    monitors
}

/// The monitor `device` names, if it is still there.
pub(crate) fn monitor(backend: BackendId, device: &str, scanout: bool) -> Option<Monitor> {
    monitors(backend, scanout)
        .into_iter()
        .find(|m| m.device == device)
}

pub(crate) const fn hwnd(handle: isize) -> HWND {
    HWND(handle as *mut c_void)
}

unsafe extern "system" fn collect_window(window: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the `&mut Vec<isize>` `window_handles` passed in,
    // alive for the whole (synchronous) enumeration.
    let handles = unsafe { &mut *(data.0 as *mut Vec<isize>) };
    handles.push(window.0 as isize);
    BOOL(1)
}

fn window_handles() -> Vec<isize> {
    let mut handles: Vec<isize> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, which outlives the
    // synchronous enumeration.
    let _all = unsafe { EnumWindows(Some(collect_window), LPARAM((&raw mut handles) as isize)) };
    handles
}

/// Longest title read, in UTF-16 units.
const TITLE_MAX: usize = 512;

/// How long a window of this process may take to answer `WM_GETTEXT`.
const OWN_TITLE_WAIT_MS: u32 = 50;

fn window_title(window: HWND, own_process: bool) -> String {
    let mut buffer = vec![0_u16; TITLE_MAX];
    let copied = if own_process {
        // A window of this process answers `WM_GETTEXT` on its own thread,
        // which may be the one waiting for this listing: ask with a
        // timeout instead of blocking on it (no title, no listing).
        let mut result = 0_usize;
        // SAFETY: WM_GETTEXT writes at most `wparam` UTF-16 units (the NUL
        // included) into `buffer`, which outlives the call.
        let sent = unsafe {
            SendMessageTimeoutW(
                window,
                WM_GETTEXT,
                WPARAM(buffer.len()),
                LPARAM(buffer.as_mut_ptr() as isize),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                OWN_TITLE_WAIT_MS,
                Some(&raw mut result),
            )
        };
        if sent.0 == 0 { 0 } else { result }
    } else {
        // SAFETY: `buffer` is live and its length is passed with it.
        // Another process's caption is read without sending it anything.
        usize::try_from(unsafe { GetWindowTextW(window, &mut buffer) }).unwrap_or(0)
    };
    buffer.truncate(copied.min(TITLE_MAX));
    String::from_utf16_lossy(&buffer)
}

fn is_cloaked(window: HWND) -> bool {
    let mut cloaked = 0_u32;
    // SAFETY: DWMWA_CLOAKED writes one DWORD into `cloaked`.
    let read = unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            u32::try_from(std::mem::size_of::<u32>()).unwrap_or(4),
        )
    };
    read.is_ok() && cloaked != 0
}

/// The window's visible frame (without the invisible resize borders),
/// physical pixels.
pub(crate) fn frame_bounds(window: isize) -> Option<Rect> {
    let mut rect = RECT::default();
    // SAFETY: DWMWA_EXTENDED_FRAME_BOUNDS writes one RECT into `rect`.
    let read = unsafe {
        DwmGetWindowAttribute(
            hwnd(window),
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rect).cast(),
            u32::try_from(std::mem::size_of::<RECT>()).unwrap_or(16),
        )
    };
    read.ok().map(|()| rect_of(rect)).filter(|r| !r.is_empty())
}

/// The executable's file name of `pid` (`notepad.exe`).
pub(crate) fn process_name(pid: u32) -> Option<String> {
    // SAFETY: opening a process for limited query rights has no memory
    // preconditions; failure is an error value.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0_u16; 1024];
    let mut len = u32::try_from(buffer.len()).unwrap_or(0);
    // SAFETY: `buffer` holds `len` u16s; `len` is updated to the length
    // written.
    let named = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &raw mut len,
        )
    };
    // SAFETY: `process` was opened above and is closed once.
    let _closed = unsafe { CloseHandle(process) };
    named.ok()?;
    let path = String::from_utf16_lossy(buffer.get(..len as usize)?);
    path.rsplit(['\\', '/']).next().and_then(non_empty)
}

/// Whether `window` is a window a person sees as an app window.
fn is_app_window(window: HWND) -> bool {
    // SAFETY: a window handle (stale ones answer false).
    if !unsafe { IsWindowVisible(window) }.as_bool() || is_cloaked(window) {
        return false;
    }
    // SAFETY: as above; extended styles are a plain integer.
    let style = unsafe { GetWindowLongW(window, GWL_EXSTYLE) }.cast_unsigned();
    let app = style & WS_EX_APPWINDOW.0 != 0;
    if style & WS_EX_TOOLWINDOW.0 != 0 && !app {
        return false;
    }
    // SAFETY: as above.
    let owned = unsafe { GetWindow(window, GW_OWNER) }.is_ok_and(|o| !o.0.is_null());
    !owned || app
}

fn describe_window(
    backend: BackendId,
    handle: isize,
    foreground: isize,
    own: u32,
) -> Option<Source> {
    let window = hwnd(handle);
    if !is_app_window(window) {
        return None;
    }
    let mut pid = 0_u32;
    // SAFETY: `pid` is a live out value.
    let _thread = unsafe { GetWindowThreadProcessId(window, Some(&raw mut pid)) };
    let title = window_title(window, pid == own);
    if title.is_empty() {
        return None;
    }
    // SAFETY: a window handle (stale ones answer false).
    let minimised = unsafe { IsIconic(window) }.as_bool();
    let geometry = frame_bounds(handle);
    if geometry.is_none() && !minimised {
        return None;
    }
    let mut affinity = 0_u32;
    // SAFETY: `affinity` is a live out value.
    let protected =
        unsafe { GetWindowDisplayAffinity(window, &raw mut affinity) }.is_ok() && affinity != 0;
    let mut source = Source::new(
        SourceId::new(backend, window_id(handle)),
        SourceKind::Window,
        title,
    );
    source.pid = (pid != 0).then_some(pid);
    source.app_id = source.pid.and_then(process_name);
    source.geometry = geometry;
    source.physical_size = geometry.map(Rect::size);
    source.scale_permille = Some(1000);
    source.flags.minimised = minimised;
    // The activated window, as on X11 and wlr.
    source.flags.primary = handle == foreground;
    source.flags.protected = protected;
    source.flags.own_process = pid == own;
    Some(source)
}

/// Every app window, top of the z-order first. `capture` is how the
/// backend captures them.
pub(crate) fn windows(backend: BackendId, capture: &SourceCapture) -> Vec<Source> {
    let _pixels = PhysicalPixels::enter();
    // SAFETY: no preconditions.
    let foreground = unsafe { GetForegroundWindow() }.0 as isize;
    // SAFETY: no preconditions.
    let own = unsafe { GetCurrentProcessId() };
    window_handles()
        .into_iter()
        .filter_map(|h| describe_window(backend, h, foreground, own))
        .map(|mut source| {
            source.capture = capture.clone();
            source
        })
        .collect()
}

/// The window `handle` as a source, if it is still an app window.
pub(crate) fn window(backend: BackendId, handle: isize) -> Option<Source> {
    let _pixels = PhysicalPixels::enter();
    // SAFETY: no preconditions.
    let foreground = unsafe { GetForegroundWindow() }.0 as isize;
    // SAFETY: no preconditions.
    let own = unsafe { GetCurrentProcessId() };
    describe_window(backend, handle, foreground, own)
}

/// Whether `handle` still names a window.
pub(crate) fn window_exists(handle: isize) -> bool {
    // SAFETY: IsWindow accepts any value.
    unsafe { IsWindow(Some(hwnd(handle))) }.as_bool()
}

/// Whether `handle` is minimised.
pub(crate) fn is_minimised(handle: isize) -> bool {
    // SAFETY: stale handles answer false.
    unsafe { IsIconic(hwnd(handle)) }.as_bool()
}

/// The foreground window, if it is a listed app window.
pub(crate) fn focused(backend: BackendId) -> Result<Source, CaptureError> {
    // SAFETY: no preconditions.
    let foreground = unsafe { GetForegroundWindow() }.0 as isize;
    window(backend, foreground)
        .ok_or_else(|| CaptureError::unsatisfiable(Some(backend), Requirement::SourceSelector))
}

/// The monitor the pointer is on.
pub(crate) fn under_cursor(backend: BackendId, scanout: bool) -> Result<Source, CaptureError> {
    let _pixels = PhysicalPixels::enter();
    let mut point = POINT::default();
    // SAFETY: `point` is a live out value.
    unsafe { GetCursorPos(&raw mut point) }
        .map_err(|e| CaptureError::os(backend, "GetCursorPos", i64::from(e.code().0)))?;
    // SAFETY: no preconditions.
    let under = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONULL) }.0 as isize;
    monitors(backend, scanout)
        .into_iter()
        .find(|m| m.hmonitor == under)
        .map(|m| m.source)
        .ok_or_else(|| CaptureError::unsatisfiable(Some(backend), Requirement::SourceSelector))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        assert_eq!(
            parse(r"monitor:\\.\DISPLAY2"),
            Some(Target::Monitor(r"\\.\DISPLAY2".to_owned()))
        );
        assert_eq!(parse(&window_id(0x1a2b)), Some(Target::Window(0x1a2b)));
        assert_eq!(parse("window:0"), None);
        assert_eq!(parse("monitor:"), None);
        assert_eq!(parse("toplevel:1"), None);
    }

    #[test]
    fn edid_manufacturer_ids_decode() {
        // LG: EDID bytes 0x1E 0x6D, read little-endian by DisplayConfig.
        assert_eq!(pnp_id(0x6D1E).as_deref(), Some("GSM"));
        // Dell: 0x10 0xAC.
        assert_eq!(pnp_id(0xAC10).as_deref(), Some("DEL"));
        assert_eq!(pnp_id(0), None);
    }

    #[test]
    fn connectors_are_named_like_on_linux() {
        assert_eq!(
            connector_name(DISPLAYCONFIG_OUTPUT_TECHNOLOGY_HDMI, 0).as_deref(),
            Some("HDMI-1")
        );
        assert_eq!(
            connector_name(DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EXTERNAL, 1).as_deref(),
            Some("DP-2")
        );
    }

    #[test]
    fn scanout_rotation_is_turned_back() {
        assert!(scanout_orientation(0).is_upright());
        assert_eq!(scanout_orientation(1).rotation, Rotation::R90);
        assert_eq!(scanout_orientation(3).rotation, Rotation::R270);
    }
}
