//! Windows [`WM_POINTER*`] subclass that decodes stylus input into
//! [`PenEvent`] / [`PenHoverEvent`] and forwards it to the per-window
//! flume channels held on [`WindowState`](super::WindowState).
//!
//! Pointer messages carry only a pointer id; the rich stylus payload
//! (pressure, tilt, twist, barrel buttons) is fetched via
//! [`GetPointerPenInfo`](windows_sys::Win32::UI::Input::Pointer::GetPointerPenInfo).
//! Coalesced samples between two `WM_POINTERUPDATE` messages come from
//! [`GetPointerPenInfoHistory`](windows_sys::Win32::UI::Input::Pointer::GetPointerPenInfoHistory).
//! Windows does not natively predict future samples; the `predicted`
//! slot on [`PenEvent::Move`] stays empty on this platform.
//!
//! The subclass forwards every message to
//! [`DefSubclassProc`](windows_sys::Win32::UI::Shell::DefSubclassProc)
//! so the host app's own input handling keeps running — this publisher
//! is a passive observer.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Instant;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::Input::Pointer::{
    GetPointerPenInfo, GetPointerPenInfoHistory, GetPointerType, POINTER_PEN_INFO,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    PT_PEN, WM_POINTERCAPTURECHANGED, WM_POINTERDOWN, WM_POINTERENTER, WM_POINTERLEAVE,
    WM_POINTERUP, WM_POINTERUPDATE,
};

use super::WindowState;
use crate::{PenButtonChange, PenEvent, PenHoverEvent, PenMove, PenSample, PenToolKind};

const SUBCLASS_ID: usize = 0x1570_0001;

const PEN_FLAG_BARREL: u32 = 0x0000_0001;
const PEN_FLAG_INVERTED: u32 = 0x0000_0002;
const PEN_FLAG_ERASER: u32 = 0x0000_0004;

const PEN_MASK_PRESSURE: u32 = 0x0000_0001;
const PEN_MASK_ROTATION: u32 = 0x0000_0002;
const PEN_MASK_TILT_X: u32 = 0x0000_0004;
const PEN_MASK_TILT_Y: u32 = 0x0000_0008;

const POINTER_FLAG_SECONDBUTTON: u32 = 0x0000_0020;

/// Sequence counter and QPC baseline shared by all events emitted for
/// a single window. Stored inline on [`WindowAttachment`] so a window
/// re-registration resets the clock.
#[derive(Debug)]
struct AttachClock {
    sequence: AtomicU32,
    start: Instant,
    tool_id_alloc: AtomicU64,
}

impl AttachClock {
    fn new() -> Self {
        Self {
            sequence: AtomicU32::new(0),
            start: Instant::now(),
            tool_id_alloc: AtomicU64::new(1),
        }
    }

    fn next_sequence(&self) -> u32 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }

    fn elapsed_us(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

/// Book-keeping returned from [`attach_hwnd`] and stashed on the
/// [`WindowState`](super::WindowState) so [`detach_hwnd`] can undo the
/// subclass installation later.
#[derive(Debug)]
pub(super) struct WindowAttachment {
    /// `HWND` stored as an integer: the raw pointer type is not `Send`,
    /// and the attachment lives inside state shared across threads.
    hwnd_id: isize,
    _state: Arc<WindowState>,
    _clock: Arc<AttachClock>,
}

/// Global registry keyed by `HWND` (as `usize`) — the subclass callback
/// receives our refdata pointer and follows it back to the right
/// [`WindowState`]. Uses `RwLock` for cheap concurrent reads on the
/// hot path.
type Registry = RwLock<Vec<Entry>>;

#[derive(Debug)]
struct Entry {
    hwnd_id: usize,
    state: Arc<WindowState>,
    clock: Arc<AttachClock>,
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

pub(super) fn attach_hwnd(
    hwnd: std::num::NonZeroIsize,
    state: Arc<WindowState>,
) -> Result<WindowAttachment, AttachError> {
    let hwnd_ptr: HWND = hwnd.get() as HWND;
    let clock = Arc::new(AttachClock::new());
    {
        let mut list = match registry().write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        list.push(Entry {
            hwnd_id: hwnd.get() as usize,
            state: Arc::clone(&state),
            clock: Arc::clone(&clock),
        });
    }
    // SAFETY: SetWindowSubclass is safe to call from any thread; the
    // subclass proc is a plain function pointer. Refdata is unused —
    // we look up state by HWND in the registry to avoid the classic
    // aliasing hazard of leaking Arc<WindowState> pointers into
    // Windows-owned memory.
    let ok = unsafe { SetWindowSubclass(hwnd_ptr, Some(subclass_proc), SUBCLASS_ID, 0) };
    if ok == 0 {
        let mut list = match registry().write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        list.retain(|e| e.hwnd_id != hwnd.get() as usize);
        return Err(AttachError::SetWindowSubclassFailed);
    }
    Ok(WindowAttachment {
        hwnd_id: hwnd.get(),
        _state: state,
        _clock: clock,
    })
}

pub(super) fn detach_hwnd(attachment: WindowAttachment) {
    let hwnd = attachment.hwnd_id as HWND;
    let hwnd_id = attachment.hwnd_id as usize;
    // SAFETY: RemoveWindowSubclass unhooks the callback for the given
    // (hwnd, proc, id) triple. Safe to call even if the HWND has been
    // destroyed — the call is a no-op.
    let _ = unsafe { RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID) };
    let mut list = match registry().write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    list.retain(|e| e.hwnd_id != hwnd_id);
}

#[derive(Debug)]
pub(super) enum AttachError {
    SetWindowSubclassFailed,
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SetWindowSubclassFailed => f.write_str("SetWindowSubclass returned FALSE"),
        }
    }
}

impl std::error::Error for AttachError {}

unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _uid_subclass: usize,
    _refdata: usize,
) -> LRESULT {
    let entry = lookup_entry(hwnd as usize);
    if let Some(entry) = entry {
        let pointer_id = get_pointer_id_from_wparam(wparam);
        if is_pointer_message(msg) && is_pen_pointer(pointer_id) {
            handle_pen_message(hwnd, msg, pointer_id, &entry);
        }
    }
    // SAFETY: forwarding to the next subclass proc in the chain is the
    // documented contract for messages we do not want to swallow.
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

fn lookup_entry(hwnd_id: usize) -> Option<Entry> {
    let list = match registry().read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    list.iter().find(|e| e.hwnd_id == hwnd_id).map(|e| Entry {
        hwnd_id: e.hwnd_id,
        state: Arc::clone(&e.state),
        clock: Arc::clone(&e.clock),
    })
}

const fn is_pointer_message(msg: u32) -> bool {
    matches!(
        msg,
        WM_POINTERDOWN
            | WM_POINTERUPDATE
            | WM_POINTERUP
            | WM_POINTERENTER
            | WM_POINTERLEAVE
            | WM_POINTERCAPTURECHANGED
    )
}

const fn get_pointer_id_from_wparam(wparam: WPARAM) -> u32 {
    (wparam as u32) & 0xFFFF
}

fn is_pen_pointer(pointer_id: u32) -> bool {
    let mut ty: i32 = 0;
    // SAFETY: GetPointerType writes a single POINTER_INPUT_TYPE (i32)
    // through the pointer.
    let ok = unsafe { GetPointerType(pointer_id, &mut ty) };
    ok != 0 && ty == PT_PEN
}

fn handle_pen_message(hwnd: HWND, msg: u32, pointer_id: u32, entry: &Entry) {
    let mut info: POINTER_PEN_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: GetPointerPenInfo fills the entire POINTER_PEN_INFO
    // structure. Zero-init first so uninitialised padding does not
    // reach later reads.
    let ok = unsafe { GetPointerPenInfo(pointer_id, &mut info) };
    if ok == 0 {
        return;
    }

    let sample = decode_sample(hwnd, &info, entry);

    match msg {
        WM_POINTERDOWN => {
            let _ = entry.state.events_tx.send(PenEvent::Down(sample));
        }
        WM_POINTERUP => {
            let _ = entry.state.events_tx.send(PenEvent::Up(sample));
        }
        WM_POINTERCAPTURECHANGED => {
            let _ = entry.state.events_tx.send(PenEvent::Cancel(sample));
        }
        WM_POINTERENTER => {
            let _ = entry
                .state
                .hover_tx
                .send(PenHoverEvent::ProximityEnter(sample));
        }
        WM_POINTERLEAVE => {
            let _ = entry.state.hover_tx.send(PenHoverEvent::ProximityLeave);
        }
        WM_POINTERUPDATE => {
            if sample.pressure > 0.0 {
                let coalesced = read_history(hwnd, pointer_id, entry);
                let _ = entry.state.events_tx.send(PenEvent::Move(PenMove {
                    sample,
                    coalesced,
                    predicted: Vec::new(),
                }));
            } else {
                let _ = entry.state.hover_tx.send(PenHoverEvent::Move(sample));
            }
        }
        _ => {}
    }
}

fn read_history(hwnd: HWND, pointer_id: u32, entry: &Entry) -> Vec<PenSample> {
    let mut count: u32 = 0;
    // SAFETY: passing a null pointer with a valid count pointer is the
    // documented way to query the required capacity.
    let ok = unsafe { GetPointerPenInfoHistory(pointer_id, &mut count, std::ptr::null_mut()) };
    if ok == 0 || count <= 1 {
        return Vec::new();
    }
    let mut buf: Vec<POINTER_PEN_INFO> = vec![unsafe { std::mem::zeroed() }; count as usize];
    // SAFETY: buf holds `count` initialised POINTER_PEN_INFO slots.
    let ok = unsafe { GetPointerPenInfoHistory(pointer_id, &mut count, buf.as_mut_ptr()) };
    if ok == 0 {
        return Vec::new();
    }
    buf.truncate(count as usize);
    // Skip the newest entry (already delivered as the live sample).
    buf.into_iter()
        .skip(1)
        .map(|info| decode_sample(hwnd, &info, entry))
        .collect()
}

fn decode_sample(hwnd: HWND, info: &POINTER_PEN_INFO, entry: &Entry) -> PenSample {
    let (x, y) = client_coords(hwnd, info.pointerInfo.ptPixelLocation);

    let pressure = if info.penMask & PEN_MASK_PRESSURE != 0 {
        (info.pressure as f32) / 1024.0
    } else {
        0.0
    };
    let tilt_x = if info.penMask & PEN_MASK_TILT_X != 0 {
        (info.tiltX as f32).to_radians()
    } else {
        0.0
    };
    let tilt_y = if info.penMask & PEN_MASK_TILT_Y != 0 {
        (info.tiltY as f32).to_radians()
    } else {
        0.0
    };
    let twist = if info.penMask & PEN_MASK_ROTATION != 0 {
        (info.rotation as f32).to_radians()
    } else {
        0.0
    };

    let tool_kind = if info.penFlags & (PEN_FLAG_ERASER | PEN_FLAG_INVERTED) != 0 {
        PenToolKind::Eraser
    } else {
        PenToolKind::Tip
    };

    let mut buttons = 0u32;
    if info.penFlags & PEN_FLAG_BARREL != 0 {
        buttons |= 1;
    }
    if info.pointerInfo.pointerFlags & POINTER_FLAG_SECONDBUTTON != 0 {
        buttons |= 1 << 1;
    }

    let tool_id = derive_tool_id(info, entry);

    PenSample {
        x,
        y,
        pressure,
        tilt_x,
        tilt_y,
        azimuth: 0.0,
        altitude: 0.0,
        twist,
        tangential_pressure: 0.0,
        z_offset: 0.0,
        timestamp_us: entry.clock.elapsed_us(),
        sequence: entry.clock.next_sequence(),
        tool_id,
        tool_kind,
        buttons,
    }
}

fn derive_tool_id(info: &POINTER_PEN_INFO, entry: &Entry) -> u32 {
    let raw = info.pointerInfo.sourceDevice as usize as u64;
    if raw == 0 {
        entry.clock.tool_id_alloc.fetch_add(1, Ordering::Relaxed) as u32
    } else {
        // Fold the source-device handle into a 32-bit id — collisions
        // across devices are theoretically possible but astronomically
        // unlikely in practice.
        ((raw ^ (raw >> 32)) & 0xFFFF_FFFF) as u32
    }
}

fn client_coords(hwnd: HWND, screen: POINT) -> (f32, f32) {
    let mut pt = screen;
    // SAFETY: ScreenToClient converts the point in-place from screen
    // to client coordinates for the given HWND.
    let ok = unsafe { ScreenToClient(hwnd, &mut pt) };
    if ok == 0 {
        (screen.x as f32, screen.y as f32)
    } else {
        (pt.x as f32, pt.y as f32)
    }
}
