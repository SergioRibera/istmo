//! X11 `XInput2` backend.
//!
//! Opens an **independent** `x11rb` connection to the running X server
//! (via `$DISPLAY`), queries XI2 ≥ 2.0, enumerates slave-pointer devices
//! whose name looks like a tablet stylus / eraser, and selects
//! `XI_Motion` / `XI_ButtonPress` / `XI_ButtonRelease` /
//! `XI_HierarchyChanged` on the hosting app's window. Decoded samples
//! route into [`PenEvent`] / [`PenHoverEvent`] on the owning
//! [`WindowState`].
//!
//! Opening a fresh connection is deliberate — sharing the Xlib
//! `Display*` would require libX11-xcb glue and tight locking against
//! the UI toolkit's own event loop. XI2 happily delivers events
//! selected on another client's window as long as the server allows
//! it; a dedicated connection also keeps the pump thread off the
//! toolkit's connection lock.
//!
//! Coordinate space: `event_x` / `event_y` arrive as `FP1616` (16.16
//! fixed point) in window-local pixels. Pressure / tilt arrive as
//! valuator classes identified by atom (`Abs Pressure`, `Abs Tilt X`,
//! `Abs Tilt Y`) and need per-device min/max normalisation.
//!
//! Current limitations (future work, tracked under M3 open questions):
//! - Device identification uses a name heuristic plus
//!   `DeviceClassData::Valuator` label atoms; probes without those
//!   atoms (e.g. `Huion` / `XP-Pen` drivers that only label "Rel X" /
//!   "Rel Y") fall through as mouse-ish and are skipped.
//! - Barrel buttons collapse to bit 1 for parity with the Wayland
//!   backend; consumer code treats the bitmask as opaque beyond "any
//!   extra button pressed".

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::struct_excessive_bools
)]

use std::collections::HashMap;
use std::ffi::c_ulong;
use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use x11rb::connection::Connection;
use x11rb::protocol::xinput::{
    ConnectionExt as _, DeviceClassData, DeviceType, EventMask, Fp1616, Fp3232, HierarchyMask,
    XIDeviceInfo, XIEventMask,
};
use x11rb::protocol::xproto::{Atom, ConnectionExt as _, Window};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use super::WindowState;
use crate::{PenButtonChange, PenEvent, PenHoverEvent, PenMove, PenSample, PenToolKind};

/// Book-keeping returned from either attach entry point.
#[derive(Debug)]
pub(super) struct XInputAttachment {
    _window: Window,
}

#[derive(Debug)]
pub(super) enum AttachError {
    /// Could not open an `x11rb` connection to the X server
    /// (`$DISPLAY` unset, auth missing, …).
    ConnectFailed(String),
    /// XI2 not advertised by the server, or version ≥ 2.0 refused.
    XiUnsupported(String),
    /// `XIQueryDevice` / `XIListProperties` / `XISelectEvents` failed.
    XiRequestFailed(String),
    /// `std::thread::Builder::spawn` failed.
    ThreadSpawn(String),
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConnectFailed(msg) => write!(f, "x11 connect failed: {msg}"),
            Self::XiUnsupported(msg) => write!(f, "XInput2 unsupported: {msg}"),
            Self::XiRequestFailed(msg) => write!(f, "XInput2 request failed: {msg}"),
            Self::ThreadSpawn(msg) => write!(f, "xinput2 pump thread spawn failed: {msg}"),
        }
    }
}

impl std::error::Error for AttachError {}

/// Attach an Xlib `Window` + `Display*` pair.
///
/// Both the Xlib and XCB entry points funnel into the same
/// implementation after narrowing to the X11 window id — we open our
/// own connection either way, so the display / xcb-connection pointer
/// is unused at runtime (kept in the return value for debug / future
/// use when we switch to shared-connection mode).
pub(super) fn attach_xlib(
    window: c_ulong,
    _display: NonZeroUsize,
    state: Arc<WindowState>,
) -> Result<XInputAttachment, AttachError> {
    let window_id = Window::from(window as u32);
    attach_window(window_id, state)
}

/// Attach an XCB `xcb_window_t` + `xcb_connection_t*` pair.
pub(super) fn attach_xcb(
    window: NonZeroU32,
    _connection: NonZeroUsize,
    state: Arc<WindowState>,
) -> Result<XInputAttachment, AttachError> {
    let window_id = Window::from(window.get());
    attach_window(window_id, state)
}

fn attach_window(
    window: Window,
    state: Arc<WindowState>,
) -> Result<XInputAttachment, AttachError> {
    let (conn, _screen) = RustConnection::connect(None)
        .map_err(|err| AttachError::ConnectFailed(err.to_string()))?;

    // XI2 handshake. 2.0 is enough for `XI_Motion` + `XI_ButtonPress`.
    let version = conn
        .xinput_xi_query_version(2, 0)
        .map_err(|err| AttachError::XiUnsupported(err.to_string()))?
        .reply()
        .map_err(|err| AttachError::XiUnsupported(err.to_string()))?;
    if version.major_version < 2 {
        return Err(AttachError::XiUnsupported(format!(
            "server advertised XI {}.{} (need ≥ 2.0)",
            version.major_version, version.minor_version
        )));
    }

    let atoms = AxisAtoms::intern(&conn)?;
    let (devices, mut device_ids) = enumerate_tablet_devices(&conn, &atoms)?;
    if device_ids.is_empty() {
        // Nothing to select, but keep the pump — HierarchyChanged
        // handling will pick up devices plugged in later.
    }
    device_ids.sort_unstable();
    device_ids.dedup();

    select_events(&conn, window, &device_ids)?;

    let mut pump_state = PumpState {
        state,
        clock: Arc::new(AttachClock::new()),
        window,
        atoms,
        devices,
        tools: HashMap::new(),
    };

    std::thread::Builder::new()
        .name("istmo-pen-xinput2".into())
        .spawn(move || loop {
            match conn.wait_for_event() {
                Ok(event) => handle_event(&conn, &mut pump_state, event),
                Err(err) => {
                    eprintln!("istmo-pen: xinput2 pump exited: {err}");
                    break;
                }
            }
        })
        .map_err(|err| AttachError::ThreadSpawn(err.to_string()))?;

    Ok(XInputAttachment { _window: window })
}

fn select_events(
    conn: &RustConnection,
    window: Window,
    device_ids: &[u16],
) -> Result<(), AttachError> {
    let axis_mask = XIEventMask::MOTION
        | XIEventMask::BUTTON_PRESS
        | XIEventMask::BUTTON_RELEASE
        | XIEventMask::ENTER
        | XIEventMask::LEAVE;

    // Always subscribe to Hierarchy on the all-devices sentinel so
    // we notice hot-plug after attach completes.
    let mut masks = vec![EventMask {
        deviceid: 0, // XIAllDevices
        mask: vec![XIEventMask::HIERARCHY],
    }];
    for id in device_ids {
        masks.push(EventMask {
            deviceid: *id,
            mask: vec![axis_mask],
        });
    }
    conn.xinput_xi_select_events(window, &masks)
        .map_err(|err| AttachError::XiRequestFailed(err.to_string()))?
        .check()
        .map_err(|err| AttachError::XiRequestFailed(err.to_string()))?;
    conn.flush()
        .map_err(|err| AttachError::XiRequestFailed(err.to_string()))?;
    Ok(())
}

/// Clock + sequence bookkeeping shared with the macOS / Wayland
/// backends.
#[derive(Debug)]
struct AttachClock {
    sequence: AtomicU32,
    start: Instant,
}

impl AttachClock {
    fn new() -> Self {
        Self {
            sequence: AtomicU32::new(0),
            start: Instant::now(),
        }
    }

    fn next_sequence(&self) -> u32 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }

    fn elapsed_us(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

#[derive(Debug)]
struct AxisAtoms {
    pressure: Atom,
    tilt_x: Atom,
    tilt_y: Atom,
}

impl AxisAtoms {
    fn intern(conn: &RustConnection) -> Result<Self, AttachError> {
        let pressure = intern_atom(conn, b"Abs Pressure")?;
        let tilt_x = intern_atom(conn, b"Abs Tilt X")?;
        let tilt_y = intern_atom(conn, b"Abs Tilt Y")?;
        Ok(Self {
            pressure,
            tilt_x,
            tilt_y,
        })
    }
}

fn intern_atom(conn: &RustConnection, name: &[u8]) -> Result<Atom, AttachError> {
    let cookie = conn
        .intern_atom(false, name)
        .map_err(|err| AttachError::XiRequestFailed(err.to_string()))?;
    let reply = cookie
        .reply()
        .map_err(|err| AttachError::XiRequestFailed(err.to_string()))?;
    Ok(reply.atom)
}

/// Per-device axis mapping + per-tool normalisation range.
#[derive(Debug, Clone)]
struct DeviceInfo {
    tool_id: u32,
    kind: PenToolKind,
    pressure_axis: Option<AxisRange>,
    tilt_x_axis: Option<AxisRange>,
    tilt_y_axis: Option<AxisRange>,
}

#[derive(Debug, Clone, Copy)]
struct AxisRange {
    number: u16,
    min: f64,
    max: f64,
}

fn fp3232_to_f64(v: Fp3232) -> f64 {
    f64::from(v.integral) + (f64::from(v.frac) / f64::from(u32::MAX))
}

fn enumerate_tablet_devices(
    conn: &RustConnection,
    atoms: &AxisAtoms,
) -> Result<(HashMap<u16, DeviceInfo>, Vec<u16>), AttachError> {
    // deviceid=0 is XIAllDevices in XI2.
    let cookie = conn
        .xinput_xi_query_device(0u16)
        .map_err(|err| AttachError::XiRequestFailed(err.to_string()))?;
    let devices: Vec<XIDeviceInfo> = cookie
        .reply()
        .map_err(|err| AttachError::XiRequestFailed(err.to_string()))?
        .infos;

    let mut map = HashMap::new();
    let mut ids = Vec::new();
    for dev in devices {
        if dev.type_ != DeviceType::SLAVE_POINTER || !dev.enabled {
            continue;
        }
        let name_lower = String::from_utf8_lossy(&dev.name).to_lowercase();
        let kind = if name_lower.contains("eraser") {
            PenToolKind::Eraser
        } else if name_lower.contains("stylus")
            || name_lower.contains(" pen")
            || name_lower.ends_with(" pen")
            || name_lower.contains("pencil")
        {
            PenToolKind::Tip
        } else {
            continue;
        };

        let mut info = DeviceInfo {
            tool_id: u32::from(dev.deviceid),
            kind,
            pressure_axis: None,
            tilt_x_axis: None,
            tilt_y_axis: None,
        };
        for class in &dev.classes {
            if let DeviceClassData::Valuator(v) = &class.data {
                let range = AxisRange {
                    number: v.number,
                    min: fp3232_to_f64(v.min),
                    max: fp3232_to_f64(v.max),
                };
                if v.label == atoms.pressure {
                    info.pressure_axis = Some(range);
                } else if v.label == atoms.tilt_x {
                    info.tilt_x_axis = Some(range);
                } else if v.label == atoms.tilt_y {
                    info.tilt_y_axis = Some(range);
                }
            }
        }
        ids.push(dev.deviceid);
        map.insert(dev.deviceid, info);
    }
    Ok((map, ids))
}

/// Pump-side state owned exclusively by the pump thread after spawn.
#[derive(Debug)]
struct PumpState {
    state: Arc<WindowState>,
    clock: Arc<AttachClock>,
    window: Window,
    atoms: AxisAtoms,
    devices: HashMap<u16, DeviceInfo>,
    tools: HashMap<u16, ToolState>,
}

#[derive(Debug)]
struct ToolState {
    tool_id: u32,
    kind: PenToolKind,
    x: f32,
    y: f32,
    pressure: f32,
    tilt_x: f32,
    tilt_y: f32,
    buttons: u32,
    in_contact: bool,
    prev_contact: bool,
    in_proximity: bool,
    prev_proximity: bool,
}

impl ToolState {
    const fn new(tool_id: u32, kind: PenToolKind) -> Self {
        Self {
            tool_id,
            kind,
            x: 0.0,
            y: 0.0,
            pressure: 0.0,
            tilt_x: 0.0,
            tilt_y: 0.0,
            buttons: 0,
            in_contact: false,
            prev_contact: false,
            in_proximity: false,
            prev_proximity: false,
        }
    }
}

fn handle_event(conn: &RustConnection, pump: &mut PumpState, event: Event) {
    match event {
        Event::XinputMotion(ev) => {
            if ev.event != pump.window {
                return;
            }
            apply_axes(pump, ev.deviceid, &ev.valuator_mask, &ev.axisvalues);
            update_position(pump, ev.deviceid, ev.event_x, ev.event_y);
            emit_move(pump, ev.deviceid);
        }
        Event::XinputButtonPress(ev) => {
            if ev.event != pump.window {
                return;
            }
            apply_axes(pump, ev.deviceid, &ev.valuator_mask, &ev.axisvalues);
            update_position(pump, ev.deviceid, ev.event_x, ev.event_y);
            handle_button(pump, ev.deviceid, ev.detail, true);
        }
        Event::XinputButtonRelease(ev) => {
            if ev.event != pump.window {
                return;
            }
            apply_axes(pump, ev.deviceid, &ev.valuator_mask, &ev.axisvalues);
            update_position(pump, ev.deviceid, ev.event_x, ev.event_y);
            handle_button(pump, ev.deviceid, ev.detail, false);
        }
        Event::XinputEnter(ev) => {
            if ev.event != pump.window {
                return;
            }
            handle_proximity(pump, ev.deviceid, true);
        }
        Event::XinputLeave(ev) => {
            if ev.event != pump.window {
                return;
            }
            handle_proximity(pump, ev.deviceid, false);
        }
        Event::XinputHierarchy(ev) => {
            let changed = ev.flags.contains(HierarchyMask::DEVICE_ENABLED)
                || ev.flags.contains(HierarchyMask::DEVICE_DISABLED)
                || ev.flags.contains(HierarchyMask::SLAVE_ADDED)
                || ev.flags.contains(HierarchyMask::SLAVE_REMOVED);
            if changed {
                refresh_devices(conn, pump);
            }
        }
        _ => {}
    }
}

fn refresh_devices(conn: &RustConnection, pump: &mut PumpState) {
    let Ok((devices, mut ids)) = enumerate_tablet_devices(conn, &pump.atoms) else {
        return;
    };
    pump.devices = devices;
    // Drop tool state for devices that vanished.
    pump.tools.retain(|dev_id, _| pump.devices.contains_key(dev_id));
    ids.sort_unstable();
    ids.dedup();
    let _ = select_events(conn, pump.window, &ids);
}

fn fp1616_to_f32(v: Fp1616) -> f32 {
    (f64::from(v) / 65536.0) as f32
}

/// Resolve or lazily create the per-device tool state. Takes a mutable
/// `&mut HashMap` + the device info by value so the caller can split
/// borrows against `pump.clock` / `pump.state` safely.
fn ensure_tool(
    tools: &mut HashMap<u16, ToolState>,
    deviceid: u16,
    info_lookup: impl FnOnce() -> Option<(u32, PenToolKind)>,
) -> Option<&mut ToolState> {
    use std::collections::hash_map::Entry;
    match tools.entry(deviceid) {
        Entry::Occupied(entry) => Some(entry.into_mut()),
        Entry::Vacant(entry) => {
            let (tool_id, kind) = info_lookup()?;
            Some(entry.insert(ToolState::new(tool_id, kind)))
        }
    }
}

fn update_position(pump: &mut PumpState, deviceid: u16, event_x: Fp1616, event_y: Fp1616) {
    let devices = &pump.devices;
    if let Some(tool) = ensure_tool(&mut pump.tools, deviceid, || {
        devices.get(&deviceid).map(|d| (d.tool_id, d.kind))
    }) {
        tool.x = fp1616_to_f32(event_x);
        tool.y = fp1616_to_f32(event_y);
    }
}

fn apply_axes(pump: &mut PumpState, deviceid: u16, mask: &[u32], values: &[Fp3232]) {
    let Some(dev) = pump.devices.get(&deviceid).cloned() else {
        return;
    };
    let mut axis_lookup: Vec<f64> = Vec::with_capacity(values.len());
    // Reconstruct dense axis-number → value map from the sparse mask.
    let mut cursor = 0;
    for (word_idx, word) in mask.iter().enumerate() {
        for bit in 0..32 {
            if (word & (1u32 << bit)) != 0 {
                let axis_num = (word_idx as u16) * 32 + bit as u16;
                while axis_lookup.len() <= axis_num as usize {
                    axis_lookup.push(f64::NAN);
                }
                let value = values.get(cursor).copied().unwrap_or(Fp3232 {
                    integral: 0,
                    frac: 0,
                });
                axis_lookup[axis_num as usize] = fp3232_to_f64(value);
                cursor += 1;
            }
        }
    }

    let Some(tool) = ensure_tool(&mut pump.tools, deviceid, || Some((dev.tool_id, dev.kind)))
    else {
        return;
    };
    if let Some(axis) = dev.pressure_axis {
        if let Some(raw) = axis_lookup.get(axis.number as usize).copied() {
            if !raw.is_nan() {
                let span = axis.max - axis.min;
                let norm = if span > 0.0 {
                    (raw - axis.min) / span
                } else {
                    0.0
                };
                tool.pressure = norm.clamp(0.0, 1.0) as f32;
            }
        }
    }
    if let Some(axis) = dev.tilt_x_axis {
        if let Some(raw) = axis_lookup.get(axis.number as usize).copied() {
            if !raw.is_nan() {
                tool.tilt_x = (raw as f32).to_radians();
            }
        }
    }
    if let Some(axis) = dev.tilt_y_axis {
        if let Some(raw) = axis_lookup.get(axis.number as usize).copied() {
            if !raw.is_nan() {
                tool.tilt_y = (raw as f32).to_radians();
            }
        }
    }
}

fn handle_proximity(pump: &mut PumpState, deviceid: u16, entering: bool) {
    let devices = &pump.devices;
    let clock = &pump.clock;
    let state = &pump.state;
    let Some(tool) = ensure_tool(&mut pump.tools, deviceid, || {
        devices.get(&deviceid).map(|d| (d.tool_id, d.kind))
    }) else {
        return;
    };
    tool.in_proximity = entering;
    let sample = build_sample(tool, clock);
    if entering && !tool.prev_proximity {
        let _ = state.hover_tx.send(PenHoverEvent::ProximityEnter(sample));
    } else if !entering && tool.prev_proximity {
        let _ = state.hover_tx.send(PenHoverEvent::ProximityLeave);
    }
    tool.prev_proximity = entering;
}

fn handle_button(pump: &mut PumpState, deviceid: u16, detail: u32, pressed: bool) {
    let devices = &pump.devices;
    let clock = &pump.clock;
    let state = &pump.state;
    let Some(tool) = ensure_tool(&mut pump.tools, deviceid, || {
        devices.get(&deviceid).map(|d| (d.tool_id, d.kind))
    }) else {
        return;
    };
    // XI2 reports the stylus tip as button 1. Treat button 1 as
    // down/up, every other button as a side-button bit.
    if detail == 1 {
        tool.in_contact = pressed;
        let sample = build_sample(tool, clock);
        if pressed && !tool.prev_contact {
            let _ = state.events_tx.send(PenEvent::Down(sample));
        } else if !pressed && tool.prev_contact {
            let _ = state.events_tx.send(PenEvent::Up(sample));
        }
        tool.prev_contact = pressed;
    } else {
        let bit = 1u32 << 1;
        let next = if pressed {
            tool.buttons | bit
        } else {
            tool.buttons & !bit
        };
        if next != tool.buttons {
            let changed = tool.buttons ^ next;
            tool.buttons = next;
            let sample = build_sample(tool, clock);
            let _ = state
                .events_tx
                .send(PenEvent::ButtonChanged(PenButtonChange { sample, changed }));
        }
    }
}

fn emit_move(pump: &mut PumpState, deviceid: u16) {
    let devices = &pump.devices;
    let clock = &pump.clock;
    let state = &pump.state;
    let Some(tool) = ensure_tool(&mut pump.tools, deviceid, || {
        devices.get(&deviceid).map(|d| (d.tool_id, d.kind))
    }) else {
        return;
    };
    let sample = build_sample(tool, clock);
    if tool.in_contact {
        let _ = state.events_tx.send(PenEvent::Move(PenMove {
            sample,
            coalesced: Vec::new(),
            predicted: Vec::new(),
        }));
    } else if tool.in_proximity {
        let _ = state.hover_tx.send(PenHoverEvent::Move(sample));
    }
}

fn build_sample(tool: &ToolState, clock: &AttachClock) -> PenSample {
    PenSample {
        x: tool.x,
        y: tool.y,
        pressure: tool.pressure,
        tilt_x: tool.tilt_x,
        tilt_y: tool.tilt_y,
        azimuth: 0.0,
        altitude: 0.0,
        twist: 0.0,
        tangential_pressure: 0.0,
        z_offset: 0.0,
        timestamp_us: clock.elapsed_us(),
        sequence: clock.next_sequence(),
        tool_id: tool.tool_id,
        tool_kind: tool.kind,
        buttons: tool.buttons,
    }
}
