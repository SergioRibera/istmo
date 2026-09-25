//! macOS AppKit-side stylus capture.
//!
//! Installs a single process-wide `NSEvent` local monitor that
//! intercepts tablet-generated `NSEvent`s and dispatches them into the
//! matching per-window [`WindowState`](super::WindowState) using the
//! `NSWindow` pointer stashed at register time.
//!
//! The monitor is passive: the handler block always returns the event
//! unchanged so ordinary AppKit dispatch keeps flowing.

use std::os::raw::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Instant;

use block2::{Block, RcBlock};
use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};

use super::WindowState;
use crate::{PenEvent, PenHoverEvent, PenMove, PenSample, PenToolKind};

// NSEventMask bit values (see AppKit's NSEvent.h).
const NS_EVENT_MASK_LEFT_MOUSE_DOWN: u64 = 1 << 1;
const NS_EVENT_MASK_LEFT_MOUSE_UP: u64 = 1 << 2;
const NS_EVENT_MASK_MOUSE_MOVED: u64 = 1 << 5;
const NS_EVENT_MASK_LEFT_MOUSE_DRAGGED: u64 = 1 << 6;
const NS_EVENT_MASK_TABLET_POINT: u64 = 1 << 23;
const NS_EVENT_MASK_TABLET_PROXIMITY: u64 = 1 << 24;

const MASK: u64 = NS_EVENT_MASK_LEFT_MOUSE_DOWN
    | NS_EVENT_MASK_LEFT_MOUSE_UP
    | NS_EVENT_MASK_LEFT_MOUSE_DRAGGED
    | NS_EVENT_MASK_MOUSE_MOVED
    | NS_EVENT_MASK_TABLET_POINT
    | NS_EVENT_MASK_TABLET_PROXIMITY;

// NSEventType (Objective-C `NSEventType` enum).
const NS_EVENT_TYPE_LEFT_MOUSE_DOWN: u64 = 1;
const NS_EVENT_TYPE_LEFT_MOUSE_UP: u64 = 2;
const NS_EVENT_TYPE_MOUSE_MOVED: u64 = 5;
const NS_EVENT_TYPE_LEFT_MOUSE_DRAGGED: u64 = 6;
const NS_EVENT_TYPE_TABLET_POINT: u64 = 23;
const NS_EVENT_TYPE_TABLET_PROXIMITY: u64 = 24;

// NSEventSubtype for a mouse event backed by a tablet stylus.
const NS_EVENT_SUBTYPE_TABLET_POINT: i16 = 1;
const NS_EVENT_SUBTYPE_TABLET_PROXIMITY: i16 = 2;

// NSPointingDeviceType.
const NS_POINTING_DEVICE_TYPE_ERASER: u64 = 2;
const NS_POINTING_DEVICE_TYPE_PEN: u64 = 1;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct NsPoint {
    x: f64,
    y: f64,
}

// SAFETY: `NsPoint` is `#[repr(C)]` with the exact field layout of
// `CGPoint` (two `CGFloat`s, which are `f64` on every 64-bit Apple
// target), matching the encoding declared here.
unsafe impl objc2::Encode for NsPoint {
    const ENCODING: objc2::Encoding =
        objc2::Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}

/// Book-keeping returned from [`attach_view`] and stashed on the
/// [`WindowState`](super::WindowState) so [`detach_view`] can drop the
/// entry when the app unregisters the window.
#[derive(Debug)]
pub(super) struct WindowAttachment {
    ns_window_id: usize,
    _clock: Arc<AttachClock>,
}

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

struct Entry {
    ns_window_id: usize,
    state: Arc<WindowState>,
    clock: Arc<AttachClock>,
}

impl std::fmt::Debug for Entry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Entry")
            .field("ns_window_id", &self.ns_window_id)
            .finish_non_exhaustive()
    }
}

type Registry = RwLock<Vec<Entry>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

pub(super) fn attach_view(
    ns_view: std::ptr::NonNull<c_void>,
    state: Arc<WindowState>,
) -> Result<WindowAttachment, AttachError> {
    // SAFETY: `NSView` responds to `-window` returning the containing
    // `NSWindow` (or nil if unattached). We only need the pointer as
    // an identifier — never dereference it.
    let ns_window: *mut AnyObject = unsafe {
        let view = ns_view.as_ptr() as *mut AnyObject;
        msg_send![view, window]
    };
    if ns_window.is_null() {
        return Err(AttachError::ViewNotAttachedToWindow);
    }
    ensure_monitor_installed()?;
    let clock = Arc::new(AttachClock::new());
    {
        let mut list = match registry().write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        list.push(Entry {
            ns_window_id: ns_window as usize,
            state,
            clock: Arc::clone(&clock),
        });
    }
    Ok(WindowAttachment {
        ns_window_id: ns_window as usize,
        _clock: clock,
    })
}

pub(super) fn detach_view(attachment: WindowAttachment) {
    let mut list = match registry().write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    list.retain(|e| e.ns_window_id != attachment.ns_window_id);
}

#[derive(Debug)]
pub(super) enum AttachError {
    ViewNotAttachedToWindow,
    NsEventClassUnavailable,
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ViewNotAttachedToWindow => {
                f.write_str("NSView is not attached to an NSWindow yet")
            }
            Self::NsEventClassUnavailable => f.write_str("NSEvent class lookup failed"),
        }
    }
}

impl std::error::Error for AttachError {}

fn ensure_monitor_installed() -> Result<(), AttachError> {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    let mut err = Ok(());
    INSTALLED.get_or_init(|| match install_monitor() {
        Ok(()) => {}
        Err(e) => err = Err(e),
    });
    err
}

fn install_monitor() -> Result<(), AttachError> {
    // SAFETY: `NSEvent` is a documented AppKit class; the local monitor
    // registration returns a retained token we intentionally leak —
    // the monitor lives for the process lifetime.
    unsafe {
        let cls = AnyClass::get(c"NSEvent").ok_or(AttachError::NsEventClassUnavailable)?;
        let handler: RcBlock<dyn Fn(*mut AnyObject) -> *mut AnyObject> =
            RcBlock::new(|event: *mut AnyObject| -> *mut AnyObject {
                dispatch_event(event);
                // Return the event unchanged — passive observer.
                event
            });
        let handler_block: &Block<dyn Fn(*mut AnyObject) -> *mut AnyObject> = &handler;
        let _monitor: *mut AnyObject = msg_send![
            cls,
            addLocalMonitorForEventsMatchingMask: MASK,
            handler: handler_block,
        ];
    }
    Ok(())
}

fn dispatch_event(event: *mut AnyObject) {
    if event.is_null() {
        return;
    }
    // SAFETY: NSEvent method sends against a non-null receiver.
    let ns_window: *mut AnyObject = unsafe { msg_send![event, window] };
    if ns_window.is_null() {
        return;
    }
    let window_id = ns_window as usize;
    let entry_snapshot = {
        let list = match registry().read() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        list.iter()
            .find(|e| e.ns_window_id == window_id)
            .map(|e| (Arc::clone(&e.state), Arc::clone(&e.clock)))
    };
    let Some((state, clock)) = entry_snapshot else {
        return;
    };
    // SAFETY: NSEvent reads on a live receiver.
    let ns_type: u64 = unsafe { msg_send![event, type] };
    match ns_type {
        NS_EVENT_TYPE_TABLET_POINT => handle_tablet_point(event, &state, &clock),
        NS_EVENT_TYPE_TABLET_PROXIMITY => handle_tablet_proximity(event, &state, &clock),
        NS_EVENT_TYPE_LEFT_MOUSE_DOWN
        | NS_EVENT_TYPE_LEFT_MOUSE_UP
        | NS_EVENT_TYPE_LEFT_MOUSE_DRAGGED
        | NS_EVENT_TYPE_MOUSE_MOVED => {
            if is_tablet_backed(event) {
                handle_mouse_from_tablet(event, ns_type, &state, &clock);
            }
        }
        _ => {}
    }
}

fn is_tablet_backed(event: *mut AnyObject) -> bool {
    // SAFETY: NSEvent read.
    let subtype: i16 = unsafe { msg_send![event, subtype] };
    subtype == NS_EVENT_SUBTYPE_TABLET_POINT
}

fn handle_mouse_from_tablet(
    event: *mut AnyObject,
    ns_type: u64,
    state: &Arc<WindowState>,
    clock: &Arc<AttachClock>,
) {
    let sample = decode_sample(event, clock);
    match ns_type {
        NS_EVENT_TYPE_LEFT_MOUSE_DOWN => {
            let _ = state.events_tx.send(PenEvent::Down(sample));
        }
        NS_EVENT_TYPE_LEFT_MOUSE_UP => {
            let _ = state.events_tx.send(PenEvent::Up(sample));
        }
        NS_EVENT_TYPE_LEFT_MOUSE_DRAGGED => {
            let _ = state.events_tx.send(PenEvent::Move(PenMove {
                sample,
                coalesced: Vec::new(),
                predicted: Vec::new(),
            }));
        }
        NS_EVENT_TYPE_MOUSE_MOVED => {
            let _ = state.hover_tx.send(PenHoverEvent::Move(sample));
        }
        _ => {}
    }
}

fn handle_tablet_point(event: *mut AnyObject, state: &Arc<WindowState>, clock: &Arc<AttachClock>) {
    // NSTabletPoint carries the richest signal but no click state —
    // fold it into a Move on the event stream. Rely on the mouse-backed
    // events for down/up transitions.
    let sample = decode_sample(event, clock);
    let _ = state.events_tx.send(PenEvent::Move(PenMove {
        sample,
        coalesced: Vec::new(),
        predicted: Vec::new(),
    }));
}

fn handle_tablet_proximity(
    event: *mut AnyObject,
    state: &Arc<WindowState>,
    clock: &Arc<AttachClock>,
) {
    // SAFETY: NSEvent read.
    let entering: bool = unsafe { msg_send![event, isEnteringProximity] };
    if entering {
        let sample = decode_sample(event, clock);
        let _ = state.hover_tx.send(PenHoverEvent::ProximityEnter(sample));
    } else {
        let _ = state.hover_tx.send(PenHoverEvent::ProximityLeave);
    }
}

fn decode_sample(event: *mut AnyObject, clock: &Arc<AttachClock>) -> PenSample {
    // SAFETY: every message send below targets a live NSEvent whose
    // methods are documented in AppKit.
    unsafe {
        let location: NsPoint = msg_send![event, locationInWindow];
        let pressure: f32 = msg_send![event, pressure];
        let tilt: NsPoint = msg_send![event, tilt];
        let rotation: f32 = msg_send![event, rotation];
        let tangential: f32 = msg_send![event, tangentialPressure];
        let device_id: u64 = msg_send![event, deviceID];
        let device_type: u64 = msg_send![event, pointingDeviceType];
        let button_mask: u64 = msg_send![event, buttonMask];

        let tool_kind = match device_type {
            NS_POINTING_DEVICE_TYPE_ERASER => PenToolKind::Eraser,
            NS_POINTING_DEVICE_TYPE_PEN => PenToolKind::Tip,
            _ => PenToolKind::Unknown,
        };

        // Rotation reported in degrees (0..360) per NSEvent docs.
        let twist = (rotation as f32).to_radians();

        // AppKit tilt already reported in normalized -1..1; convert to
        // radians of tilt-from-perpendicular assuming full-scale = PI/2.
        let tilt_x = (tilt.x as f32) * std::f32::consts::FRAC_PI_2;
        let tilt_y = (tilt.y as f32) * std::f32::consts::FRAC_PI_2;

        let tool_id = if device_id == 0 {
            clock.tool_id_alloc.fetch_add(1, Ordering::Relaxed) as u32
        } else {
            (device_id & 0xFFFF_FFFF) as u32
        };

        let mut buttons = 0u32;
        if button_mask & 0x2 != 0 {
            buttons |= 1; // secondary tip button
        }
        if button_mask & 0x4 != 0 {
            buttons |= 1 << 1; // eraser / third button
        }

        PenSample {
            x: location.x as f32,
            y: location.y as f32,
            pressure,
            tilt_x,
            tilt_y,
            azimuth: 0.0,
            altitude: 0.0,
            twist,
            tangential_pressure: tangential,
            z_offset: 0.0,
            timestamp_us: clock.elapsed_us(),
            sequence: clock.next_sequence(),
            tool_id,
            tool_kind,
            buttons,
        }
    }
}
