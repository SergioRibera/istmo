//! Windows: the notification area (`Shell_NotifyIconW`,
//! `NOTIFYICON_VERSION_4`) and popup menus (`TrackPopupMenuEx`).
//!
//! One thread owns a hidden top-level window and its message loop. It
//! is not a message-only window: those miss the `TaskbarCreated`
//! broadcast sent when explorer.exe restarts, after which every icon is
//! added again. Menus are built from the model each time they open, so
//! menu changes need no work until then.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Weak};

use istmo_core::NativeHandleId;
use istmo_menu::{CheckState, ItemId, ItemKind, Label, Menu, MenuNode, Modifiers, Shortcut};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BI_BITFIELDS, BITMAPINFO, BITMAPV5HEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, HBITMAP, HMONITOR, InvalidateRect, MONITOR_DEFAULTTONEAREST, MonitorFromPoint,
    MonitorFromRect,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, GetSystemMetricsForDpi,
    MDT_EFFECTIVE_DPI, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::Shell::{
    NIF_GUID, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_STATE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, NINF_KEY, NIS_HIDDEN, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, NOTIFYICONIDENTIFIER, Shell_NotifyIconGetRect, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateIconIndirect, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyIcon, DestroyMenu, DispatchMessageW, EnumThreadWindows, GetClassNameW,
    GetMenuItemInfoW, GetMessageW, GetSystemMetrics, GetWindowThreadProcessId, HICON, HMENU,
    ICONINFO, InsertMenuItemW, MENUITEMINFOW, MFS_CHECKED, MFS_DEFAULT, MFS_DISABLED, MFS_HILITE,
    MFT_RADIOCHECK, MFT_SEPARATOR, MFT_STRING, MIIM_BITMAP, MIIM_FTYPE, MIIM_ID, MIIM_STATE,
    MIIM_STRING, MIIM_SUBMENU, MSG, MSGFLT_ALLOW, PostMessageW, PostQuitMessage, RegisterClassExW,
    RegisterWindowMessageW, SM_CXSMICON, SM_MENUDROPALIGNMENT, SetForegroundWindow,
    SetMenuItemInfoW, TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTALIGN, TPM_RIGHTBUTTON,
    TPM_VERTICAL, TPMPARAMS, TrackPopupMenuEx, TranslateMessage, WM_APP, WM_CONTEXTMENU,
    WM_DISPLAYCHANGE, WM_DPICHANGED, WM_LBUTTONDBLCLK, WM_MBUTTONUP, WM_NULL, WM_SETTINGCHANGE,
    WNDCLASSEXW, WS_OVERLAPPED,
};
use windows_sys::core::{BOOL, GUID};

use crate::host::{Backend, Dirty, MenuDirty, Shared};
use crate::image::{DecodedIcon, Raster};
use crate::{
    Anchor, AnchorPrecision, EventSupport, HostState, MenuSupport, Pointer, PrimaryAction, Rect,
    Support, TrayBackend, TrayCapabilities, TrayError, TrayEvent,
};

/// Posted by other threads: pending changes to apply.
const WM_WAKE: u32 = WM_APP + 1;
/// The icons' callback message.
const WM_TRAY: u32 = WM_APP + 2;
/// Posted by `Drop`: remove the icons and end the loop.
const WM_STOP: u32 = WM_APP + 3;
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;
const TIP_CHARS: usize = 127;

struct Win {
    /// The hidden window, as an integer (`HWND` is not `Send`).
    hwnd: usize,
}

impl Win {
    fn post(&self, message: u32) {
        // SAFETY: posting to a window handle is defined for any value;
        // the window lives until `WM_STOP` is handled, and a post to a
        // destroyed window only fails.
        let _posted = unsafe { PostMessageW(self.hwnd as HWND, message, 0, 0) };
    }
}

impl Backend for Win {
    fn capabilities(&self) -> TrayCapabilities {
        capabilities()
    }

    fn wake(&self) {
        self.post(WM_WAKE);
    }
}

impl Drop for Win {
    fn drop(&mut self) {
        self.post(WM_STOP);
    }
}

pub(super) fn start(shared: &Arc<Shared>) -> Result<Arc<dyn Backend>, TrayError> {
    let (tx, rx) = oneshot::channel();
    let weak = Arc::downgrade(shared);
    std::thread::Builder::new()
        .name("istmo-tray-win".to_owned())
        .spawn(move || {
            // Per-monitor DPI for this thread's window and menus, whatever
            // the process declared: icons and menus are drawn at each
            // monitor's DPI instead of being stretched by the system.
            // Fails (null) before Windows 10 1703, which keeps the
            // process's awareness.
            // SAFETY: changes only this thread's awareness context.
            let _previous =
                unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
            let hwnd = match create_window() {
                Ok(hwnd) => hwnd,
                Err(err) => {
                    let _sent = tx.send(Err(err));
                    return;
                }
            };
            UI.with(|ui| *ui.borrow_mut() = Some(Ui::new(weak, hwnd)));
            let _sent = tx.send(Ok(hwnd as usize));
            message_loop();
        })
        .map_err(|err| TrayError::Platform(err.to_string()))?;
    let hwnd = rx.recv().map_err(|_| TrayError::ShutDown)??;
    shared.set_host(HostState::Available);
    Ok(Arc::new(Win { hwnd }))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn create_window() -> Result<HWND, TrayError> {
    let class = wide("IstmoTrayWindow");
    // SAFETY: a null module name asks for the executable's own handle.
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSEXW {
        cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).unwrap_or(u32::MAX),
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        ..zeroed_class()
    };
    // SAFETY: `wc` is fully initialised and `class` outlives the call.
    // Registering twice (a second host in one process) fails with
    // "class already exists", which `CreateWindowExW` does not mind.
    let _atom = unsafe { RegisterClassExW(&raw const wc) };
    let title = wide("istmo tray");
    // SAFETY: the class is registered on this thread's module and the
    // strings outlive the call; no parent, menu or creation data.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        return Err(TrayError::Platform(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    Ok(hwnd)
}

const fn zeroed_class() -> WNDCLASSEXW {
    WNDCLASSEXW {
        cbSize: 0,
        style: 0,
        lpfnWndProc: None,
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: std::ptr::null_mut(),
        hIcon: std::ptr::null_mut(),
        hCursor: std::ptr::null_mut(),
        hbrBackground: std::ptr::null_mut(),
        lpszMenuName: std::ptr::null(),
        lpszClassName: std::ptr::null(),
        hIconSm: std::ptr::null_mut(),
    }
}

fn message_loop() {
    // SAFETY: `MSG` is plain data; all-zero is a valid value.
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `msg` is a valid out pointer; null window = every
        // window of this thread.
        let got = unsafe { GetMessageW(&raw mut msg, std::ptr::null_mut(), 0, 0) };
        if got <= 0 {
            return;
        }
        // SAFETY: `msg` was filled by `GetMessageW`.
        let _translated = unsafe { TranslateMessage(&raw const msg) };
        // SAFETY: as above.
        let _result = unsafe { DispatchMessageW(&raw const msg) };
    }
}

thread_local! {
    /// The window thread's state. Never borrowed across a call that
    /// runs a nested message loop (`TrackPopupMenuEx`).
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    /// The menu being shown. Separate from `UI` so changes reach it
    /// from inside the menu's own message loop.
    static OPEN: RefCell<Option<OpenMenu>> = const { RefCell::new(None) };
}

/// One icon as the shell knows it.
struct Added {
    uid: u32,
    guid: Option<GUID>,
}

struct Ui {
    shared: Weak<Shared>,
    hwnd: HWND,
    taskbar_created: u32,
    added: HashMap<NativeHandleId, Added>,
    by_uid: HashMap<u32, NativeHandleId>,
    icons: IconCache,
    /// Notification area icon size, in pixels.
    size: u32,
}

impl Ui {
    fn new(shared: Weak<Shared>, hwnd: HWND) -> Self {
        let name = wide("TaskbarCreated");
        // SAFETY: `name` is a NUL-terminated string that outlives the call.
        let taskbar_created = unsafe { RegisterWindowMessageW(name.as_ptr()) };
        // Elevated processes only get the broadcast through the filter.
        // SAFETY: `hwnd` is this thread's window; no out structure.
        let _allowed = unsafe {
            ChangeWindowMessageFilterEx(hwnd, taskbar_created, MSGFLT_ALLOW, std::ptr::null_mut())
        };
        Self {
            shared,
            hwnd,
            taskbar_created,
            added: HashMap::new(),
            by_uid: HashMap::new(),
            icons: IconCache::default(),
            size: tray_icon_size(),
        }
    }

    fn apply_pending(&mut self) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        for (tray, dirty) in shared.take_pending() {
            if dirty.menu != MenuDirty::Clean {
                sync_open_menu(&shared, tray);
            }
            self.apply(&shared, tray, &dirty);
        }
        self.icons.purge();
    }

    fn apply(&mut self, shared: &Shared, tray: NativeHandleId, dirty: &Dirty) {
        if dirty.removed {
            if let Some(added) = self.added.remove(&tray) {
                self.by_uid.remove(&added.uid);
                let mut nid = self.data(&added);
                // SAFETY: `nid` identifies an icon this window added.
                let _deleted = unsafe { Shell_NotifyIconW(NIM_DELETE, &raw mut nid) };
            }
            if !dirty.added {
                return;
            }
        }
        let size = self.size;
        let state = shared.lock();
        let Some(entry) = state.trays.get(&tray) else {
            return;
        };
        let icon = entry.icon.frame().clone();
        let tip = tooltip(entry.tooltip.as_ref(), &entry.id);
        let visible = entry.visible;
        let guid = entry.windows_guid.as_deref().and_then(parse_guid);
        let uid = entry.serial.wrapping_add(1);
        drop(state);

        let adding = dirty.added || !self.added.contains_key(&tray);
        let added = self.added.entry(tray).or_insert(Added { uid, guid });
        let mut nid = data_for(self.hwnd, added);
        nid.uFlags |= NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP | NIF_STATE;
        nid.uCallbackMessage = WM_TRAY;
        nid.hIcon = self.icons.get(&icon, size).unwrap_or(std::ptr::null_mut());
        copy_tip(&mut nid.szTip, &tip);
        nid.dwStateMask = NIS_HIDDEN;
        nid.dwState = if visible { 0 } else { NIS_HIDDEN };
        if adding {
            if !add_icon(&mut nid) {
                // Explorer is not running (an app started before the
                // shell) or refused the icon. `TaskbarCreated` adds it
                // again once a taskbar exists.
                tracing::warn!("tray: Shell_NotifyIconW(NIM_ADD) failed");
                self.added.remove(&tray);
                if self.added.is_empty() {
                    shared.set_host(HostState::Missing);
                }
                return;
            }
            self.by_uid.insert(uid, tray);
            nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            // SAFETY: as above; `uVersion` is the active union field
            // for `NIM_SETVERSION`.
            let _versioned = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &raw mut nid) };
        } else if dirty.icon || dirty.tooltip || dirty.visible {
            // SAFETY: `nid` identifies an icon this window added.
            let _modified = unsafe { Shell_NotifyIconW(NIM_MODIFY, &raw mut nid) };
        }
    }

    fn data(&self, added: &Added) -> NOTIFYICONDATAW {
        data_for(self.hwnd, added)
    }

    /// Every icon again when the icon size changed (DPI change, display
    /// change). `WM_SETTINGCHANGE` comes often and rarely changes it.
    fn refresh_icons(&mut self) {
        let size = tray_icon_size();
        if size == self.size {
            return;
        }
        self.size = size;
        self.icons.clear();
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        let trays: Vec<_> = self.added.keys().copied().collect();
        let icon_only = Dirty {
            icon: true,
            ..Dirty::default()
        };
        for tray in trays {
            self.apply(&shared, tray, &icon_only);
        }
    }

    fn anchor(&self, tray: NativeHandleId) -> Option<Anchor> {
        let rect = self.icon_rect(tray)?;
        Some(Anchor {
            rect: Rect {
                x: rect.left,
                y: rect.top,
                width: u32::try_from(rect.right - rect.left).unwrap_or(0),
                height: u32::try_from(rect.bottom - rect.top).unwrap_or(0),
            },
            // SAFETY: plain query; any rectangle has a nearest monitor.
            scale: scale(unsafe { MonitorFromRect(&raw const rect, MONITOR_DEFAULTTONEAREST) }),
            precision: AnchorPrecision::IconRect,
        })
    }

    /// The icon's rectangle on screen, in physical pixels.
    fn icon_rect(&self, tray: NativeHandleId) -> Option<RECT> {
        let added = self.added.get(&tray)?;
        let identifier = NOTIFYICONIDENTIFIER {
            cbSize: u32::try_from(size_of::<NOTIFYICONIDENTIFIER>()).unwrap_or(u32::MAX),
            hWnd: self.hwnd,
            uID: added.uid,
            guidItem: added.guid.unwrap_or(GUID::from_u128(0)),
        };
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: both pointers are valid for the call.
        let hr = unsafe { Shell_NotifyIconGetRect(&raw const identifier, &raw mut rect) };
        (hr >= 0).then_some(rect)
    }
}

/// `NIM_ADD`, once more after deleting a stale icon: a GUID still held
/// by a run that never removed its icon (a crash) refuses the add.
fn add_icon(nid: &mut NOTIFYICONDATAW) -> bool {
    // SAFETY: `nid` is fully initialised for this thread's window.
    if unsafe { Shell_NotifyIconW(NIM_ADD, nid) } != 0 {
        return true;
    }
    if nid.uFlags & NIF_GUID == 0 {
        return false;
    }
    // SAFETY: as above; deleting an icon that does not exist only fails.
    let _deleted = unsafe { Shell_NotifyIconW(NIM_DELETE, nid) };
    // SAFETY: as above.
    unsafe { Shell_NotifyIconW(NIM_ADD, nid) != 0 }
}

fn data_for(hwnd: HWND, added: &Added) -> NOTIFYICONDATAW {
    // SAFETY: `NOTIFYICONDATAW` is plain data; all-zero is valid.
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = u32::try_from(size_of::<NOTIFYICONDATAW>()).unwrap_or(u32::MAX);
    nid.hWnd = hwnd;
    nid.uID = added.uid;
    if let Some(guid) = added.guid {
        nid.uFlags |= NIF_GUID;
        nid.guidItem = guid;
    }
    nid
}

const BASE_DPI: u32 = 96;

/// The monitor's effective DPI (96 when unknown).
fn monitor_dpi(monitor: HMONITOR) -> u32 {
    let (mut x, mut y) = (0, 0);
    // SAFETY: both out pointers are valid; a null or stale monitor only
    // fails.
    let hr = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut x, &raw mut y) };
    if hr < 0 || x == 0 { BASE_DPI } else { x }
}

fn scale(monitor: HMONITOR) -> f64 {
    f64::from(monitor_dpi(monitor)) / f64::from(BASE_DPI)
}

fn monitor_at(point: POINT) -> HMONITOR {
    // SAFETY: plain query; any point has a nearest monitor.
    unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) }
}

/// The small icon size at `dpi`.
fn small_icon(dpi: u32) -> u32 {
    // SAFETY: plain query.
    let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) };
    u32::try_from(size).unwrap_or(16).max(16)
}

/// The notification area's icon size: the primary monitor's (its
/// origin is always 0, 0), where the taskbar with the icons is. Read
/// from the monitor, not the system DPI, which stays at its sign-in
/// value until the user signs in again.
fn tray_icon_size() -> u32 {
    small_icon(monitor_dpi(monitor_at(POINT { x: 0, y: 0 })))
}

fn tooltip(tooltip: Option<&crate::Tooltip>, fallback: &str) -> String {
    tooltip.map_or_else(
        || fallback.to_owned(),
        |tip| {
            tip.body.as_ref().map_or_else(
                || tip.title.clone(),
                |body| format!("{}\n{body}", tip.title),
            )
        },
    )
}

/// Copies whole characters only: a cut surrogate pair shows as a box.
/// `slot` is zeroed, so the text stays NUL-terminated.
fn copy_tip(slot: &mut [u16; 128], text: &str) {
    let mut at = 0;
    for c in text.chars() {
        let mut units = [0; 2];
        let units = c.encode_utf16(&mut units);
        let Some(dst) = slot[..TIP_CHARS].get_mut(at..at + units.len()) else {
            break;
        };
        dst.copy_from_slice(units);
        at += units.len();
    }
}

/// `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`, braces allowed.
fn parse_guid(text: &str) -> Option<GUID> {
    let hex: String = text
        .trim_matches(|c| c == '{' || c == '}')
        .chars()
        .filter(|&c| c != '-')
        .collect();
    if hex.len() != 32 {
        tracing::warn!(%text, "tray: windows_guid is not a GUID; using the icon id");
        return None;
    }
    u128::from_str_radix(&hex, 16).ok().map(GUID::from_u128)
}

/// `HICON`s per decoded icon and size, freed once the icon is gone.
#[derive(Default)]
struct IconCache {
    entries: HashMap<(usize, u32), (Weak<DecodedIcon>, HICON)>,
}

impl IconCache {
    fn get(&mut self, icon: &Arc<DecodedIcon>, size: u32) -> Option<HICON> {
        let key = (Arc::as_ptr(icon) as usize, size);
        if let Some((cached, hicon)) = self.entries.get(&key) {
            // A dead entry's address may belong to a new icon.
            if cached.upgrade().is_some_and(|c| Arc::ptr_eq(&c, icon)) {
                return Some(*hicon);
            }
        }
        let hicon = create_icon(&*icon.raster(size)?)?;
        if let Some((_, stale)) = self.entries.insert(key, (Arc::downgrade(icon), hicon)) {
            destroy_icon(stale);
        }
        Some(hicon)
    }

    fn purge(&mut self) {
        self.entries.retain(|_, (icon, hicon)| {
            let alive = icon.strong_count() > 0;
            if !alive {
                destroy_icon(*hicon);
            }
            alive
        });
    }

    fn clear(&mut self) {
        for (_, (_, hicon)) in self.entries.drain() {
            destroy_icon(hicon);
        }
    }
}

fn destroy_icon(hicon: HICON) {
    // SAFETY: the cache owns `hicon`, created by `CreateIconIndirect`;
    // the shell copies icons, so destroying ours is always safe.
    let _destroyed = unsafe { DestroyIcon(hicon) };
}

/// A 32 bpp top-down DIB section with `pixels` (BGRA, premultiplied
/// or not as the caller decides).
fn dib(width: u32, height: u32, pixels: &[u8]) -> Option<HBITMAP> {
    let header = BITMAPV5HEADER {
        bV5Size: u32::try_from(size_of::<BITMAPV5HEADER>()).unwrap_or(u32::MAX),
        bV5Width: i32::try_from(width).ok()?,
        bV5Height: -i32::try_from(height).ok()?,
        bV5Planes: 1,
        bV5BitCount: 32,
        bV5Compression: BI_BITFIELDS,
        bV5RedMask: 0x00FF_0000,
        bV5GreenMask: 0x0000_FF00,
        bV5BlueMask: 0x0000_00FF,
        bV5AlphaMask: 0xFF00_0000,
        // SAFETY: the rest of the header is plain data; zero is valid.
        ..unsafe { std::mem::zeroed() }
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    // SAFETY: a `BITMAPV5HEADER` may be passed where a `BITMAPINFO` is
    // expected (its size field says which); `bits` receives the buffer.
    let bitmap = unsafe {
        CreateDIBSection(
            std::ptr::null_mut(),
            (&raw const header).cast::<BITMAPINFO>(),
            DIB_RGB_COLORS,
            &raw mut bits,
            std::ptr::null_mut(),
            0,
        )
    };
    if bitmap.is_null() || bits.is_null() {
        return None;
    }
    // SAFETY: the section holds `width * height * 4` bytes, as many as
    // `pixels`, and nothing else uses it yet.
    unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len()) };
    Some(bitmap)
}

fn delete_bitmap(bitmap: HBITMAP) {
    // SAFETY: the caller owns `bitmap` and no DC has it selected.
    let _deleted = unsafe { DeleteObject(bitmap) };
}

fn bgra(raster: &Raster, premultiply: bool) -> Vec<u8> {
    raster
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&[r, g, b, a]| {
            if premultiply {
                let scale = |c: u8| u8::try_from(u16::from(c) * u16::from(a) / 255).unwrap_or(c);
                [scale(b), scale(g), scale(r), a]
            } else {
                [b, g, r, a]
            }
        })
        .collect()
}

fn create_icon(raster: &Raster) -> Option<HICON> {
    let color = dib(raster.width, raster.height, &bgra(raster, false))?;
    // An all-zero AND mask: the colour bitmap's alpha decides.
    let stride = raster.width.div_ceil(16) as usize * 2;
    let mask_bits = vec![0u8; stride * raster.height as usize];
    // SAFETY: `mask_bits` holds one word-aligned row per scan line.
    let mask = unsafe {
        CreateBitmap(
            i32::try_from(raster.width).ok()?,
            i32::try_from(raster.height).ok()?,
            1,
            1,
            mask_bits.as_ptr().cast(),
        )
    };
    let info = ICONINFO {
        fIcon: 1,
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: color,
    };
    // SAFETY: `info` holds two live bitmaps; the icon copies them.
    let hicon = unsafe { CreateIconIndirect(&raw const info) };
    delete_bitmap(color);
    if !mask.is_null() {
        delete_bitmap(mask);
    }
    (!hicon.is_null()).then_some(hicon)
}

/// The dash shown in place of a check mark for `Mixed`.
fn mixed_glyph(size: u32) -> Raster {
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let thickness = (size / 8).max(1);
    let top = (size - thickness) / 2;
    for y in top..top + thickness {
        for x in size / 4..size - size / 4 {
            let at = ((y * size + x) * 4) as usize;
            rgba[at..at + 4].copy_from_slice(&[0, 0, 0, 255]);
        }
    }
    Raster {
        width: size,
        height: size,
        rgba,
    }
}

fn shortcut_text(shortcut: &Shortcut) -> String {
    let mut parts = Vec::new();
    for (bit, name) in [
        (Modifiers::CONTROL, "Ctrl"),
        (Modifiers::ALT, "Alt"),
        (Modifiers::SHIFT, "Shift"),
        (Modifiers::SUPER, "Win"),
    ] {
        if shortcut.modifiers.contains(bit) {
            parts.push(name);
        }
    }
    parts.push(&shortcut.key);
    parts.join("+")
}

/// Where an item's bitmap comes from: it is replaced only when this
/// changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bitmap {
    None,
    Mixed,
    /// An item icon, by the decoded icon's address.
    Icon(usize),
}

/// How one item looks: what `BuiltMenu::level` inserts and
/// `BuiltMenu::sync` compares against.
struct Look {
    text: Vec<u16>,
    ftype: u32,
    state: u32,
    bitmap: Bitmap,
}

impl Look {
    fn of(node: &MenuNode, icons: &HashMap<ItemId, Arc<DecodedIcon>>) -> Self {
        let mut ftype = 0;
        let mut state = 0;
        if !node.enabled || matches!(node.kind, ItemKind::Header) {
            state |= MFS_DISABLED;
        }
        if node.is_default {
            state |= MFS_DEFAULT;
        }
        let mut mixed = false;
        match node.kind {
            ItemKind::Separator => ftype = MFT_SEPARATOR,
            ItemKind::Check(check) => match check.state {
                CheckState::On => state |= MFS_CHECKED,
                CheckState::Off => {}
                // `Mixed` (and states added later): a dash in the icon
                // column, unless the item has an icon of its own.
                _ => mixed = true,
            },
            ItemKind::Radio(radio) => {
                ftype = MFT_RADIOCHECK;
                if radio.selected {
                    state |= MFS_CHECKED;
                }
            }
            // Actions, submenus, headers, and kinds added later.
            _ => {}
        }
        let text = if matches!(node.kind, ItemKind::Separator) {
            Vec::new()
        } else {
            ftype |= MFT_STRING;
            let mut label = Label::parse(&node.label).with_marker('&');
            if let Some(shortcut) = &node.shortcut {
                label.push('\t');
                label.push_str(&shortcut_text(shortcut));
            }
            wide(&label)
        };
        let bitmap = match icons.get(&node.id) {
            Some(icon) => Bitmap::Icon(Arc::as_ptr(icon) as usize),
            None if mixed => Bitmap::Mixed,
            None => Bitmap::None,
        };
        Self {
            text,
            ftype,
            state,
            bitmap,
        }
    }
}

fn menu_item_info() -> MENUITEMINFOW {
    // SAFETY: `MENUITEMINFOW` is plain data; all-zero is valid.
    let mut item: MENUITEMINFOW = unsafe { std::mem::zeroed() };
    item.cbSize = u32::try_from(size_of::<MENUITEMINFOW>()).unwrap_or(u32::MAX);
    item
}

/// A popup menu built from the model, and the GDI objects it uses.
struct BuiltMenu {
    menu: HMENU,
    bitmaps: Vec<HBITMAP>,
    /// The items inserted, as they look now.
    items: HashMap<ItemId, Look>,
    /// Item bitmap size, in pixels.
    size: u32,
}

impl Drop for BuiltMenu {
    fn drop(&mut self) {
        if !self.menu.is_null() {
            // SAFETY: `menu` was created by `CreatePopupMenu` and is no
            // longer shown; destroying it destroys its submenus.
            let _destroyed = unsafe { DestroyMenu(self.menu) };
        }
        for bitmap in self.bitmaps.drain(..) {
            delete_bitmap(bitmap);
        }
    }
}

impl BuiltMenu {
    fn build(menu: &Menu, icons: &HashMap<ItemId, Arc<DecodedIcon>>, size: u32) -> Option<Self> {
        let mut built = Self {
            menu: std::ptr::null_mut(),
            bitmaps: Vec::new(),
            items: HashMap::new(),
            size,
        };
        built.menu = built.level(menu, None, icons)?;
        Some(built)
    }

    fn level(
        &mut self,
        menu: &Menu,
        parent: Option<ItemId>,
        icons: &HashMap<ItemId, Arc<DecodedIcon>>,
    ) -> Option<HMENU> {
        // SAFETY: no arguments; the caller owns the returned menu.
        let hmenu = unsafe { CreatePopupMenu() };
        if hmenu.is_null() {
            return None;
        }
        let mut position = 0u32;
        for node in menu.children(parent).filter(|n| n.visible) {
            let Some(id) = node.id.value.checked_add(1) else {
                continue;
            };
            let mut look = Look::of(node, icons);
            let mut item = menu_item_info();
            item.fMask = MIIM_ID | MIIM_FTYPE | MIIM_STATE;
            item.wID = id;
            item.fType = look.ftype;
            item.fState = look.state;
            if !look.text.is_empty() {
                item.fMask |= MIIM_STRING;
                item.dwTypeData = look.text.as_mut_ptr();
            }
            if matches!(node.kind, ItemKind::Submenu) {
                // A submenu that cannot be created is left out.
                let Some(submenu) = self.level(menu, Some(node.id), icons) else {
                    continue;
                };
                item.fMask |= MIIM_SUBMENU;
                item.hSubMenu = submenu;
            }
            if let Some(bitmap) = self.bitmap(node.id, look.bitmap, icons) {
                item.fMask |= MIIM_BITMAP;
                item.hbmpItem = bitmap;
            }
            // SAFETY: `item` is initialised, `look.text` outlives the
            // call (the menu copies it) and `hmenu` is ours.
            let _inserted = unsafe { InsertMenuItemW(hmenu, position, 1, &raw const item) };
            self.items.insert(node.id, look);
            position += 1;
        }
        Some(hmenu)
    }

    /// The bitmap for `source`, owned by this menu.
    fn bitmap(
        &mut self,
        id: ItemId,
        source: Bitmap,
        icons: &HashMap<ItemId, Arc<DecodedIcon>>,
    ) -> Option<HBITMAP> {
        let bitmap = match source {
            Bitmap::None => return None,
            Bitmap::Mixed => {
                let glyph = mixed_glyph(self.size);
                dib(glyph.width, glyph.height, &bgra(&glyph, true))
            }
            Bitmap::Icon(_) => {
                let raster = icons.get(&id)?.raster(self.size)?;
                dib(raster.width, raster.height, &bgra(&raster, true))
            }
        }?;
        self.bitmaps.push(bitmap);
        Some(bitmap)
    }

    /// Sets the shown items' changed properties from the model. Items
    /// added, removed, shown or hidden since the menu opened wait for
    /// the next open: a shown menu keeps its layout.
    fn sync(&mut self, menu: &Menu, icons: &HashMap<ItemId, Arc<DecodedIcon>>) {
        for node in menu.nodes() {
            let Some(id) = node.id.value.checked_add(1) else {
                continue;
            };
            let mut look = Look::of(node, icons);
            let Some(shown) = self.items.get(&node.id) else {
                continue;
            };
            let text_changed = look.text != shown.text;
            let ftype_changed = look.ftype != shown.ftype;
            let state_changed = look.state != shown.state;
            let bitmap_changed = look.bitmap != shown.bitmap;
            if !(text_changed || ftype_changed || state_changed || bitmap_changed) {
                continue;
            }
            let mut item = menu_item_info();
            if text_changed {
                item.fMask |= MIIM_STRING;
                item.dwTypeData = look.text.as_mut_ptr();
            }
            if ftype_changed {
                item.fMask |= MIIM_FTYPE;
                item.fType = look.ftype;
            }
            if state_changed {
                item.fMask |= MIIM_STATE;
                item.fState = look.state | (self.state(id) & MFS_HILITE);
            }
            if bitmap_changed {
                item.fMask |= MIIM_BITMAP;
                item.hbmpItem = self
                    .bitmap(node.id, look.bitmap, icons)
                    .unwrap_or(std::ptr::null_mut());
            }
            // SAFETY: `item` is initialised and `look.text` outlives the
            // call; by command (`FALSE`) also searches the submenus.
            let _set = unsafe { SetMenuItemInfoW(self.menu, id, 0, &raw const item) };
            self.items.insert(node.id, look);
        }
    }

    /// The item's state now: replacing it would drop the highlight of
    /// the item under the pointer.
    fn state(&self, id: u32) -> u32 {
        let mut item = menu_item_info();
        item.fMask = MIIM_STATE;
        // SAFETY: `item` is initialised with its size; the call only
        // writes `fState`.
        let ok = unsafe { GetMenuItemInfoW(self.menu, id, 0, &raw mut item) };
        if ok == 0 { 0 } else { item.fState }
    }
}

/// The menu being shown: which tray it belongs to and the window that
/// owns it.
struct OpenMenu {
    tray: NativeHandleId,
    owner: HWND,
    built: BuiltMenu,
}

/// Applies the model to the tray's menu if it is the one being shown.
fn sync_open_menu(shared: &Shared, tray: NativeHandleId) {
    OPEN.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return;
        };
        let Some(open) = slot.as_mut().filter(|open| open.tray == tray) else {
            return;
        };
        let state = shared.lock();
        if let Some(entry) = state.trays.get(&tray) {
            open.built.sync(&entry.menu, &entry.item_icons);
        }
        drop(state);
        repaint_menus(open.owner);
    });
}

/// Repaints this thread's popup menu windows: a shown menu does not
/// redraw on its own after `SetMenuItemInfoW`.
fn repaint_menus(owner: HWND) {
    // SAFETY: plain query; no process id wanted.
    let thread = unsafe { GetWindowThreadProcessId(owner, std::ptr::null_mut()) };
    // SAFETY: the callback has the `WNDENUMPROC` signature and keeps
    // nothing past the call.
    let _enumerated = unsafe { EnumThreadWindows(thread, Some(repaint_if_menu), 0) };
}

/// `#32768`, the class of popup menu windows.
const MENU_CLASS: &[u16] = &[0x23, 0x33, 0x32, 0x37, 0x36, 0x38];

unsafe extern "system" fn repaint_if_menu(hwnd: HWND, _: LPARAM) -> BOOL {
    let mut class = [0u16; 8];
    // SAFETY: `class` holds as many units as passed.
    let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), 8) };
    let class = usize::try_from(len).ok().and_then(|len| class.get(..len));
    if class == Some(MENU_CLASS) {
        // SAFETY: `hwnd` is a live window of this thread; a null
        // rectangle is the whole client area.
        let _invalidated = unsafe { InvalidateRect(hwnd, std::ptr::null(), 1) };
    }
    1
}

/// Shows the tray's menu at `at` (avoiding `exclude`, the icon) and
/// reports the choice. Runs a nested message loop: no `UI` borrow may
/// be held.
fn show_menu(
    shared: &Arc<Shared>,
    hwnd: HWND,
    tray: NativeHandleId,
    at: POINT,
    exclude: Option<RECT>,
) {
    // A click on another icon while a menu is shown.
    if OPEN.with(|open| open.try_borrow().map_or(true, |open| open.is_some())) {
        return;
    }
    let size = small_icon(monitor_dpi(monitor_at(at)));
    let built = {
        let state = shared.lock();
        let Some(entry) = state.trays.get(&tray) else {
            return;
        };
        if entry.menu.is_empty() {
            return;
        }
        let built = BuiltMenu::build(&entry.menu, &entry.item_icons, size);
        drop(state);
        built
    };
    let Some(built) = built else {
        return;
    };
    let menu = built.menu;
    OPEN.with(|open| {
        *open.borrow_mut() = Some(OpenMenu {
            tray,
            owner: hwnd,
            built,
        });
    });
    shared.emit(tray, TrayEvent::MenuOpened);
    // Without this the menu does not close on a click elsewhere.
    // SAFETY: `hwnd` is this thread's window.
    let _foreground = unsafe { SetForegroundWindow(hwnd) };
    // SAFETY: plain query.
    let align = if unsafe { GetSystemMetrics(SM_MENUDROPALIGNMENT) } == 0 {
        TPM_LEFTALIGN
    } else {
        TPM_RIGHTALIGN
    };
    let params = exclude.map(|rect| TPMPARAMS {
        cbSize: u32::try_from(size_of::<TPMPARAMS>()).unwrap_or(u32::MAX),
        rcExclude: rect,
    });
    let flags = TPM_RETURNCMD
        | TPM_NONOTIFY
        | TPM_RIGHTBUTTON
        | align
        | if params.is_some() { TPM_VERTICAL } else { 0 };
    let params = params.as_ref().map_or(std::ptr::null(), std::ptr::from_ref);
    // SAFETY: `menu` is a live popup menu owned by this thread (kept in
    // `OPEN` until the call returns); `params` is null or points at a
    // value that outlives the call.
    let chosen = unsafe { TrackPopupMenuEx(menu, flags, at.x, at.y, hwnd, params) };
    // SAFETY: as in `Win::post`.
    let _posted = unsafe { PostMessageW(hwnd, WM_NULL, 0, 0) };
    let closed = OPEN.with(|open| open.borrow_mut().take());
    drop(closed);
    shared.emit(tray, TrayEvent::MenuClosed);
    if let Some(item) = u32::try_from(chosen).ok().and_then(|id| id.checked_sub(1)) {
        shared.activate_item(tray, ItemId::from(item));
    }
}

/// What a click on an icon asks for, decided with `UI` borrowed and
/// done after the borrow ends.
enum Click {
    /// Show the menu at the point, avoiding the icon rectangle.
    Menu {
        shared: Arc<Shared>,
        hwnd: HWND,
        tray: NativeHandleId,
        at: POINT,
        exclude: Option<RECT>,
    },
    Nothing,
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the message packs 16-bit words into `WPARAM` / `LPARAM`"
)]
fn on_tray(ui: &Ui, wparam: WPARAM, lparam: LPARAM) -> Click {
    let event = (lparam as u32) & 0xFFFF;
    let uid = ((lparam as u32) >> 16) & 0xFFFF;
    let Some(&tray) = ui.by_uid.get(&uid) else {
        return Click::Nothing;
    };
    let Some(shared) = ui.shared.upgrade() else {
        return Click::Nothing;
    };
    // Version 4 puts the event's screen point in `wparam`: the icon for
    // keyboard selection, the pointer otherwise.
    let at = POINT {
        x: i32::from(((wparam & 0xFFFF) as u16).cast_signed()),
        y: i32::from((((wparam >> 16) & 0xFFFF) as u16).cast_signed()),
    };
    let point = || {
        let anchor = ui.anchor(tray).or_else(|| {
            Some(Anchor {
                rect: Rect {
                    x: at.x,
                    y: at.y,
                    width: 0,
                    height: 0,
                },
                scale: scale(monitor_at(at)),
                precision: AnchorPrecision::ClickPoint,
            })
        });
        Pointer { anchor }
    };
    let primary = shared.lock().trays.get(&tray).map(|t| {
        (
            t.primary,
            t.menu
                .nodes()
                .iter()
                .find(|n| n.is_default && n.enabled && n.visible)
                .map(|n| n.id),
        )
    });
    let Some((primary, default_item)) = primary else {
        return Click::Nothing;
    };
    match event {
        NIN_SELECT | NIN_KEYSELECT if primary == PrimaryAction::Activate => {
            shared.emit(tray, TrayEvent::Activate(point()));
            Click::Nothing
        }
        NIN_SELECT | NIN_KEYSELECT | WM_CONTEXTMENU => Click::Menu {
            shared,
            hwnd: ui.hwnd,
            tray,
            at,
            exclude: ui.icon_rect(tray),
        },
        WM_LBUTTONDBLCLK => {
            shared.emit(tray, TrayEvent::DoubleClick(point()));
            if let Some(item) = default_item {
                shared.activate_item(tray, item);
            }
            Click::Nothing
        }
        WM_MBUTTONUP => {
            shared.emit(tray, TrayEvent::SecondaryActivate(point()));
            Click::Nothing
        }
        _ => Click::Nothing,
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let handled = UI.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return None;
        };
        let ui = slot.as_mut()?;
        if message == ui.taskbar_created && message != 0 {
            ui.added.clear();
            ui.by_uid.clear();
            if let Some(shared) = ui.shared.upgrade() {
                shared.set_host(HostState::Restarted);
                shared.set_host(HostState::Available);
                shared.readd_all();
            }
            return Some(Click::Nothing);
        }
        match message {
            WM_WAKE => {
                ui.apply_pending();
                Some(Click::Nothing)
            }
            WM_TRAY => Some(on_tray(ui, wparam, lparam)),
            WM_DISPLAYCHANGE | WM_SETTINGCHANGE | WM_DPICHANGED => {
                ui.refresh_icons();
                None
            }
            WM_STOP => {
                for added in ui.added.values() {
                    let mut nid = ui.data(added);
                    // SAFETY: `nid` identifies an icon this window added.
                    let _deleted = unsafe { Shell_NotifyIconW(NIM_DELETE, &raw mut nid) };
                }
                ui.added.clear();
                ui.icons.clear();
                // SAFETY: ends this thread's message loop.
                unsafe { PostQuitMessage(0) };
                Some(Click::Nothing)
            }
            _ => None,
        }
    });
    match handled {
        Some(Click::Menu {
            shared,
            hwnd,
            tray,
            at,
            exclude,
        }) => {
            show_menu(&shared, hwnd, tray, at, exclude);
            0
        }
        Some(Click::Nothing) => 0,
        // SAFETY: the arguments are the ones the system passed in.
        None => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

const fn capabilities() -> TrayCapabilities {
    TrayCapabilities {
        backend: TrayBackend::Win32,
        host: HostState::Available,
        max_items: None,
        min_frame_ms: 33,
        image_names: Support::Unsupported,
        template_images: Support::Unsupported,
        title: Support::Unsupported,
        tooltip_body: Support::Emulated,
        menu: MenuSupport {
            check: Support::Native,
            mixed: Support::Emulated,
            radio: Support::Native,
            item_icons: Support::Native,
            shortcuts: Support::Native,
            header: Support::Emulated,
            subtitle: Support::Unsupported,
            badge: Support::Unsupported,
            default_item: Support::Native,
            mnemonics: Support::Native,
            live_while_open: Support::Native,
        },
        events: EventSupport {
            activate: Support::Native,
            secondary: Support::Native,
            double_click: Support::Native,
            scroll: Support::Unsupported,
            menu_open_close: Support::Native,
        },
        anchor: Some(AnchorPrecision::IconRect),
    }
}
