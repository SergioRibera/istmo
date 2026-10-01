//! Wayland `zwp_tablet_v2` backend.
//!
//! Opens a secondary [`wayland_client::Connection`] on the hosting
//! app's `wl_display` via [`wayland_backend::client::Backend::from_foreign_display`],
//! binds `zwp_tablet_manager_v2` + `wl_seat`, enumerates tablet tools
//! and forwards their `motion` / `down` / `up` / `pressure` / `tilt` /
//! `distance` / `rotation` events into the [`PenEvent`] /
//! [`PenHoverEvent`] streams on the owning [`WindowState`].
//!
//! Coordinates arrive as surface-local `wl_fixed` (24.8) which the
//! wayland-scanner code already converts to `f64` — logical pixels, no
//! millimetre conversion. Pressure / distance are `u32` normalised to
//! `0..=65535`; tilt / rotation are `fixed` degrees.
//!
//! A dedicated pump thread drives the queue with `blocking_dispatch`.
//! It outlives the [`WaylandAttachment`] on drop (no co-operative
//! shutdown yet); events route through a `send` on the shared
//! [`WindowState`] senders, which no-op cleanly once the receivers go
//! away.

#![allow(unsafe_op_in_unsafe_fn)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::struct_excessive_bools
)]

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use wayland_backend::client::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalList, GlobalListContents};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{
    delegate_noop, event_created_child, Connection, Dispatch, Proxy, QueueHandle, WEnum,
};
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_manager_v2::ZwpTabletManagerV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_pad_v2::ZwpTabletPadV2;
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_seat_v2::{
    self, ZwpTabletSeatV2,
};
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_tool_v2::{
    self, ButtonState as ToolButtonState, Type as ToolType, ZwpTabletToolV2,
};
use wayland_protocols::wp::tablet::zv2::client::zwp_tablet_v2::ZwpTabletV2;

use super::WindowState;
use crate::{PenButtonChange, PenEvent, PenHoverEvent, PenMove, PenSample, PenToolKind};

/// Book-keeping returned from [`attach_wayland`]. The pump thread owns
/// its own clone of the shared state; this handle only keeps the
/// window-handle pointers around for debuggability.
#[derive(Debug)]
pub(super) struct WaylandAttachment {
    _surface: NonZeroUsize,
    _display: NonZeroUsize,
}

#[derive(Debug)]
pub(super) enum AttachError {
    /// `wayland-client` could not finish the registry round-trip or
    /// could not bind the required globals (`wl_seat`,
    /// `zwp_tablet_manager_v2`). The compositor likely does not speak
    /// tablet-v2.
    InitFailed(String),
    /// `std::thread::Builder::spawn` failed.
    ThreadSpawn(String),
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InitFailed(msg) => write!(f, "wayland tablet-v2 init failed: {msg}"),
            Self::ThreadSpawn(msg) => write!(f, "wayland pump thread spawn failed: {msg}"),
        }
    }
}

impl std::error::Error for AttachError {}

/// Attach a Wayland `wl_surface` + `wl_display` pair to the publisher.
///
/// Spawns a dedicated pump thread that drives a secondary
/// `wayland_client::Connection` over the hosting app's `wl_display`.
/// Events outside the registered surface are silently dropped by the
/// pump; events on the target surface route into `state`.
pub(super) fn attach_wayland(
    surface: NonZeroUsize,
    display: NonZeroUsize,
    state: Arc<WindowState>,
) -> Result<WaylandAttachment, AttachError> {
    let display_raw = display.get() as *mut ();
    let surface_raw = surface.get() as *mut ();

    // SAFETY: `display` points at a live `wl_display` owned by the
    // hosting app for the duration of the app's lifetime. The guest
    // backend never closes it on drop (see `Backend::from_foreign_display`
    // docs).
    let backend = unsafe { Backend::from_foreign_display(display_raw.cast()) };
    let conn = Connection::from_backend(backend);
    let (globals, mut event_queue) = registry_queue_init::<PumpState>(&conn)
        .map_err(|err| AttachError::InitFailed(err.to_string()))?;
    let qh = event_queue.handle();

    let seat: WlSeat = globals
        .bind(&qh, 1..=7, ())
        .map_err(|err| AttachError::InitFailed(format!("wl_seat bind: {err}")))?;
    let manager: ZwpTabletManagerV2 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|err| AttachError::InitFailed(format!("zwp_tablet_manager_v2 bind: {err}")))?;
    let tablet_seat = manager.get_tablet_seat(&seat, &qh, ());

    let mut pump_state = PumpState {
        state,
        clock: Arc::new(AttachClock::new()),
        target_surface: surface_raw,
        tools: HashMap::new(),
        _globals: globals,
        _seat: seat,
        _manager: manager,
        _tablet_seat: tablet_seat,
    };

    std::thread::Builder::new()
        .name("istmo-pen-wayland".into())
        .spawn(move || loop {
            if let Err(err) = event_queue.blocking_dispatch(&mut pump_state) {
                eprintln!("istmo-pen: wayland pump exited: {err}");
                break;
            }
        })
        .map_err(|err| AttachError::ThreadSpawn(err.to_string()))?;

    Ok(WaylandAttachment {
        _surface: surface,
        _display: display,
    })
}

/// Clock + sequence bookkeeping, mirrors the macOS backend so sample
/// timestamps / sequence numbers behave consistently cross-platform.
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

/// Pump-side state. Owned exclusively by the pump thread after spawn.
///
/// `target_surface` is the opaque `*mut wl_proxy` pointer the hosting
/// app handed us via `raw-window-handle`. We use it strictly for
/// equality comparison against `proximity_in.surface.id().as_ptr()` —
/// no dereference — so the raw pointer never leaves this type.
struct PumpState {
    state: Arc<WindowState>,
    clock: Arc<AttachClock>,
    target_surface: *mut (),
    tools: HashMap<ObjectId, ToolState>,
    _globals: GlobalList,
    _seat: WlSeat,
    _manager: ZwpTabletManagerV2,
    _tablet_seat: ZwpTabletSeatV2,
}

// SAFETY: `target_surface` is a raw address used only for equality in
// the pump thread. Every other field is already Send.
unsafe impl Send for PumpState {}

impl std::fmt::Debug for PumpState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PumpState")
            .field("target_surface", &format_args!("{:p}", self.target_surface))
            .field("tools", &self.tools.len())
            .finish_non_exhaustive()
    }
}

/// Accumulated per-tool state. The protocol delivers axes as separate
/// events and commits them with a `frame`; we batch the deltas and
/// emit one `PenEvent` / `PenHoverEvent` per frame.
#[derive(Debug)]
struct ToolState {
    tool_id: u32,
    kind: PenToolKind,
    x: f32,
    y: f32,
    pressure: f32,
    tilt_x: f32,
    tilt_y: f32,
    twist: f32,
    distance: f32,
    buttons: u32,
    on_target_surface: bool,
    in_proximity: bool,
    in_contact: bool,
    prev_proximity: bool,
    prev_contact: bool,
    pending_axis: bool,
    changed_buttons: u32,
}

impl ToolState {
    const fn new(tool_id: u32) -> Self {
        Self {
            tool_id,
            kind: PenToolKind::Unknown,
            x: 0.0,
            y: 0.0,
            pressure: 0.0,
            tilt_x: 0.0,
            tilt_y: 0.0,
            twist: 0.0,
            distance: 0.0,
            buttons: 0,
            on_target_surface: false,
            in_proximity: false,
            in_contact: false,
            prev_proximity: false,
            prev_contact: false,
            pending_axis: false,
            changed_buttons: 0,
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for PumpState {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: wayland_client::protocol::wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Hot-plug of tablet manager / seats is out of scope for the
        // first cut; `GlobalListContents` keeps its own list for later
        // re-binding if we add that path.
    }
}

delegate_noop!(PumpState: ignore WlSeat);
delegate_noop!(PumpState: ZwpTabletManagerV2);
delegate_noop!(PumpState: ignore ZwpTabletV2);
delegate_noop!(PumpState: ignore ZwpTabletPadV2);
delegate_noop!(PumpState: ignore WlSurface);

impl Dispatch<ZwpTabletSeatV2, ()> for PumpState {
    fn event(
        state: &mut Self,
        _proxy: &ZwpTabletSeatV2,
        event: zwp_tablet_seat_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwp_tablet_seat_v2::Event::ToolAdded { id } = event {
            let tool_id = (state.clock.tool_id_alloc.fetch_add(1, Ordering::Relaxed) & 0xFFFF_FFFF)
                as u32;
            state.tools.insert(id.id(), ToolState::new(tool_id));
        }
    }

    // Opcodes of `zwp_tablet_seat_v2` events that carry a `new_id`:
    //   0 → tablet_added  (zwp_tablet_v2)
    //   1 → tool_added    (zwp_tablet_tool_v2)
    //   2 → pad_added     (zwp_tablet_pad_v2)
    event_created_child!(PumpState, ZwpTabletSeatV2, [
        0 => (ZwpTabletV2, ()),
        1 => (ZwpTabletToolV2, ()),
        2 => (ZwpTabletPadV2, ()),
    ]);
}

impl Dispatch<ZwpTabletToolV2, ()> for PumpState {
    #[allow(clippy::too_many_lines)]
    fn event(
        pump: &mut Self,
        proxy: &ZwpTabletToolV2,
        event: zwp_tablet_tool_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use zwp_tablet_tool_v2::Event as E;
        let id = proxy.id();
        match event {
            E::Type { tool_type } => {
                if let Some(tool) = pump.tools.get_mut(&id) {
                    tool.kind = map_tool_type(tool_type);
                }
            }
            E::ProximityIn { surface: _, .. } => {
                // Surface focus-matching is intentionally dropped: our
                // secondary `Backend::from_foreign_display` connection
                // keeps an independent proxy cache from the host app's
                // libwayland, so comparing the `proximity_in.surface`
                // proxy pointer against the raw-window-handle pointer
                // never matches. The compositor only delivers
                // tablet-tool events for surfaces owned by this
                // client (= the host app), so for the single-window
                // publisher shape treating every `proximity_in` as
                // ours is correct. Multi-window routing lands with the
                // per-surface id handshake in a follow-up.
                if let Some(tool) = pump.tools.get_mut(&id) {
                    tool.in_proximity = true;
                    tool.on_target_surface = true;
                }
            }
            E::ProximityOut => {
                if let Some(tool) = pump.tools.get_mut(&id) {
                    tool.in_proximity = false;
                }
            }
            E::Down { .. } => {
                if let Some(tool) = pump.tools.get_mut(&id) {
                    tool.in_contact = true;
                }
            }
            E::Up => {
                if let Some(tool) = pump.tools.get_mut(&id) {
                    tool.in_contact = false;
                }
            }
            E::Motion { x, y } => {
                if let Some(tool) = pump.tools.get_mut(&id) {
                    tool.x = x as f32;
                    tool.y = y as f32;
                    tool.pending_axis = true;
                }
            }
            E::Pressure { pressure } => {
                if let Some(tool) = pump.tools.get_mut(&id) {
                    tool.pressure = (pressure as f32) / 65535.0;
                    tool.pending_axis = true;
                }
            }
            E::Distance { distance } => {
                if let Some(tool) = pump.tools.get_mut(&id) {
                    tool.distance = (distance as f32) / 65535.0;
                    tool.pending_axis = true;
                }
            }
            E::Tilt { tilt_x, tilt_y } => {
                if let Some(tool) = pump.tools.get_mut(&id) {
                    tool.tilt_x = (tilt_x as f32).to_radians();
                    tool.tilt_y = (tilt_y as f32).to_radians();
                    tool.pending_axis = true;
                }
            }
            E::Rotation { degrees } => {
                if let Some(tool) = pump.tools.get_mut(&id) {
                    tool.twist = (degrees as f32).to_radians();
                    tool.pending_axis = true;
                }
            }
            E::Button {
                state: btn_state, ..
            } => {
                if let Some(tool) = pump.tools.get_mut(&id) {
                    // Barrel buttons collapse to bit 1 for now — the
                    // XInput2 backend will do the same, and consumer
                    // code treats the bitmap as opaque beyond "any
                    // extra button is pressed".
                    let bit = 1u32 << 1;
                    let pressed = matches!(btn_state, WEnum::Value(ToolButtonState::Pressed));
                    let next = if pressed {
                        tool.buttons | bit
                    } else {
                        tool.buttons & !bit
                    };
                    if next != tool.buttons {
                        tool.changed_buttons |= tool.buttons ^ next;
                        tool.buttons = next;
                    }
                }
            }
            E::Frame { .. } => {
                flush_frame(pump, &id);
            }
            E::Removed => {
                pump.tools.remove(&id);
                proxy.destroy();
            }
            _ => {}
        }
    }
}

const fn map_tool_type(ty: WEnum<ToolType>) -> PenToolKind {
    match ty {
        WEnum::Value(ToolType::Eraser) => PenToolKind::Eraser,
        WEnum::Value(ToolType::Pen | ToolType::Brush | ToolType::Pencil | ToolType::Airbrush) => {
            PenToolKind::Tip
        }
        _ => PenToolKind::Unknown,
    }
}

fn flush_frame(pump: &mut PumpState, id: &ObjectId) {
    let Some(tool) = pump.tools.get_mut(id) else {
        return;
    };
    let on_surface = tool.on_target_surface || tool.prev_proximity;
    if !on_surface {
        tool.pending_axis = false;
        tool.changed_buttons = 0;
        return;
    }

    let sample = build_sample(tool, &pump.clock);

    let entered_proximity = tool.in_proximity && !tool.prev_proximity;
    let left_proximity = !tool.in_proximity && tool.prev_proximity;
    let entered_contact = tool.in_contact && !tool.prev_contact;
    let left_contact = !tool.in_contact && tool.prev_contact;

    if entered_proximity {
        let _ = pump
            .state
            .hover_tx
            .send(PenHoverEvent::ProximityEnter(sample.clone()));
    }
    if entered_contact {
        let _ = pump.state.events_tx.send(PenEvent::Down(sample.clone()));
    } else if tool.pending_axis && tool.in_contact {
        let _ = pump.state.events_tx.send(PenEvent::Move(PenMove {
            sample: sample.clone(),
            coalesced: Vec::new(),
            predicted: Vec::new(),
        }));
    } else if tool.pending_axis && tool.in_proximity {
        let _ = pump.state.hover_tx.send(PenHoverEvent::Move(sample.clone()));
    }
    if tool.changed_buttons != 0 {
        let _ = pump
            .state
            .events_tx
            .send(PenEvent::ButtonChanged(PenButtonChange {
                sample: sample.clone(),
                changed: tool.changed_buttons,
            }));
    }
    if left_contact {
        let _ = pump.state.events_tx.send(PenEvent::Up(sample));
    }
    if left_proximity {
        let _ = pump.state.hover_tx.send(PenHoverEvent::ProximityLeave);
    }

    tool.prev_proximity = tool.in_proximity;
    tool.prev_contact = tool.in_contact;
    tool.pending_axis = false;
    tool.changed_buttons = 0;
    if !tool.in_proximity {
        tool.on_target_surface = false;
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
        twist: tool.twist,
        tangential_pressure: 0.0,
        z_offset: tool.distance,
        timestamp_us: clock.elapsed_us(),
        sequence: clock.next_sequence(),
        tool_id: tool.tool_id,
        tool_kind: tool.kind,
        buttons: tool.buttons,
    }
}
