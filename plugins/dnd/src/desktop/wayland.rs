//! Wayland drag and drop on the app's own `wl_display`.
//!
//! winit binds no `wl_data_device`, so drops on Wayland never reach the
//! UI toolkit. A secondary [`Connection`] on the app's display (through
//! [`Backend::from_foreign_display`], as `istmo-pen` does for tablets)
//! binds one and follows the drag:
//!
//! - `enter` with an offer carrying `text/uri-list`, or inside Flatpak
//!   `application/vnd.portal.filetransfer`, is accepted while the app
//!   listens, and the [`DragState`] moves to `Over`;
//! - `motion` updates the position (surface-local logical pixels), a few
//!   dozen times a second at most;
//! - `drop` asks for the payload through a pipe, read on a short-lived
//!   thread so the pump keeps dispatching; the offer is finished and the
//!   paths go out as one drop.
//!
//! Dragging out of the app uses the same connection: its own `wl_pointer`
//! and `wl_touch` keep the serial and surface of the press in progress,
//! which `wl_data_device.start_drag` needs, and a `wl_data_source` hands
//! over the data when the target asks (inside Flatpak, files go through
//! the document portal as well).
//!
//! One connection serves every window of a display: the compositor sends
//! a drag over any of the app's surfaces to each data device the app
//! bound, so one device per window would deliver every drop once per
//! window.

#![allow(clippy::cast_possible_truncation)]

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::num::NonZeroUsize;
use std::os::fd::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use istmo_core::sync::lock;
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_callback::WlCallback;
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, Capability, WlSeat};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::protocol::wl_touch::{self, WlTouch};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child,
};

use super::{DragDone, Shared, files, outgoing, portal};
use crate::{DndError, DragOver, DragRequest, DragResult, DragState, DropPoint};

const URI_LIST: &str = "text/uri-list";
/// Payloads are paths or a portal key; anything bigger is not a drop.
const MAX_PAYLOAD: u64 = 1 << 20;
const MOVE_INTERVAL: Duration = Duration::from_millis(33);

/// The press in progress on one of the app's surfaces, which a drag out
/// starts from.
#[derive(Debug, Default)]
struct Press {
    serial: Option<u32>,
    surface: Option<WlSurface>,
    down: bool,
}

/// The app's windows on this display: id and `wl_surface` address.
type Windows = Arc<Mutex<Vec<(u64, usize)>>>;

/// The pump of one display. Dropping it stops and joins the thread.
#[derive(Debug)]
pub(super) struct Attachment {
    windows: Windows,
    press: Arc<Mutex<Press>>,
    manager: WlDataDeviceManager,
    device: WlDataDevice,
    sandboxed: bool,
    stop: Arc<AtomicBool>,
    conn: Connection,
    qh: QueueHandle<Pump>,
    thread: Option<JoinHandle<()>>,
}

impl Attachment {
    pub(super) fn add_window(&self, id: u64, surface: usize) {
        let mut windows = lock(&self.windows);
        windows.retain(|(known, _)| *known != id);
        windows.push((id, surface));
    }

    /// Drag `request` out from the press in progress; `done` answers when
    /// the drag ends.
    pub(super) fn start_drag(&self, request: &DragRequest, done: DragDone) {
        let (serial, surface) = {
            let press = lock(&self.press);
            match (press.down, press.serial, press.surface.clone()) {
                (true, Some(serial), Some(surface)) => (serial, surface),
                _ => return done.finish(Err(DndError::NotPressed)),
            }
        };
        let uris = outgoing::uri_list(&request.items);
        let text = outgoing::plain_text(&request.items);
        let files = outgoing::files(&request.items);
        let mut mimes: Vec<&'static str> = Vec::new();
        if uris.is_some() {
            mimes.push(URI_LIST);
        }
        if self.sandboxed && !files.is_empty() {
            mimes.push(portal::FILE_TRANSFER);
        }
        if text.is_some() {
            mimes.extend(TEXT_MIMES);
        }
        let payload = Arc::new(Outgoing {
            uris,
            text,
            files,
            portal_key: Mutex::new(None),
            done: Mutex::new(Some(done)),
        });
        let source = self
            .manager
            .create_data_source(&self.qh, Arc::clone(&payload));
        for mime in mimes {
            source.offer(mime.to_owned());
        }
        if source.version() >= 3 {
            source.set_actions(DndAction::Copy);
        }
        self.device
            .start_drag(Some(&source), &surface, None, serial);
        if let Err(err) = self.conn.flush() {
            source.destroy();
            payload.finish(Err(DndError::Backend(err.to_string())));
        }
    }

    /// Forget window `id`; `false` once no window is left, so the caller
    /// drops the attachment.
    pub(super) fn remove_window(&self, id: u64) -> bool {
        let mut windows = lock(&self.windows);
        windows.retain(|(known, _)| *known != id);
        !windows.is_empty()
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // The pump blocks in `blocking_dispatch` until its queue gets an
        // event; the compositor answers this sync on that queue.
        let _callback = self.conn.display().sync(&self.qh, ());
        if let Err(err) = self.conn.flush() {
            tracing::warn!(target: "istmo.dnd.wayland", %err, "flush on detach");
        }
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!(target: "istmo.dnd.wayland", "pump thread panicked");
        }
    }
}

/// Why the display could not be followed.
#[derive(Debug)]
pub(super) struct AttachError(String);

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "wayland drag and drop: {}", self.0)
    }
}

/// Follow drags on `display`.
pub(super) fn attach(
    display: NonZeroUsize,
    shared: Arc<Shared>,
) -> Result<Attachment, AttachError> {
    // SAFETY: `display` is the live `wl_display` of the app's windows,
    // registered with `istmo-window` and alive for as long as they are.
    // A foreign-display backend never closes it.
    let backend = unsafe { Backend::from_foreign_display(display.get() as *mut _) };
    let conn = Connection::from_backend(backend);
    let (globals, mut queue) =
        registry_queue_init::<Pump>(&conn).map_err(|err| AttachError(err.to_string()))?;
    let qh = queue.handle();
    let seat: WlSeat = globals
        .bind(&qh, 1..=7, ())
        .map_err(|err| AttachError(format!("wl_seat: {err}")))?;
    let manager: WlDataDeviceManager = globals
        .bind(&qh, 1..=3, ())
        .map_err(|err| AttachError(format!("wl_data_device_manager: {err}")))?;
    let device = manager.get_data_device(&seat, &qh, ());

    let windows = Windows::default();
    let press = Arc::new(Mutex::new(Press::default()));
    let sandboxed = portal::sandboxed();
    let stop = Arc::new(AtomicBool::new(false));
    let mut pump = Pump {
        shared,
        windows: Arc::clone(&windows),
        offers: HashMap::new(),
        drag: None,
        sandboxed,
        conn: conn.clone(),
        device: device.clone(),
        press: Arc::clone(&press),
        pointer: None,
        touch: None,
        _seat: seat,
        _manager: manager.clone(),
    };
    let pump_stop = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("istmo-dnd-wayland".into())
        .spawn(move || {
            while !pump_stop.load(Ordering::Acquire) {
                if let Err(err) = queue.blocking_dispatch(&mut pump) {
                    tracing::warn!(target: "istmo.dnd.wayland", %err, "pump exited");
                    return;
                }
            }
            // The display is the app's: release what this pump created.
            pump.shut_down();
            let _ = queue.flush();
        })
        .map_err(|err| AttachError(err.to_string()))?;
    Ok(Attachment {
        windows,
        press,
        manager,
        device,
        sandboxed,
        stop,
        conn,
        qh,
        thread: Some(thread),
    })
}

/// A drag over one of the app's surfaces.
#[derive(Debug)]
struct Drag {
    offer: Option<WlDataOffer>,
    /// The type asked for on drop; `None` when the drag is refused.
    mime: Option<&'static str>,
    window_id: Option<u64>,
    position: DropPoint,
    last_move: Instant,
}

/// Pump-side state, owned by the pump thread.
#[derive(Debug)]
struct Pump {
    shared: Arc<Shared>,
    windows: Windows,
    /// MIME types announced for each offer not yet entered or dropped.
    offers: HashMap<ObjectId, HashSet<String>>,
    drag: Option<Drag>,
    sandboxed: bool,
    conn: Connection,
    device: WlDataDevice,
    press: Arc<Mutex<Press>>,
    pointer: Option<WlPointer>,
    touch: Option<WlTouch>,
    _seat: WlSeat,
    _manager: WlDataDeviceManager,
}

impl Pump {
    fn shut_down(&mut self) {
        if let Some(drag) = self.drag.take()
            && let Some(offer) = drag.offer
        {
            offer.destroy();
        }
        if self.device.version() >= 2 {
            self.device.release();
        }
    }

    /// The window `surface` belongs to. The proxy behind an event's
    /// surface is not always the one the app registered (each
    /// connection keeps its own); with a single window that one is it.
    fn window_of(&self, surface: &WlSurface) -> Option<u64> {
        let windows = lock(&self.windows);
        let address = surface.id().as_ptr() as usize;
        windows
            .iter()
            .find(|(_, known)| *known == address)
            .or_else(|| (windows.len() == 1).then(|| &windows[0]))
            .map(|(id, _)| *id)
    }

    /// The type to ask for, out of what an offer announced.
    fn pick(&self, mimes: &HashSet<String>) -> Option<&'static str> {
        let portal = mimes.contains(portal::FILE_TRANSFER);
        let uris = mimes.contains(URI_LIST);
        // Host paths mean nothing inside the sandbox; outside it the
        // plain list needs no portal round trip.
        match (self.sandboxed, portal, uris) {
            (true, true, _) | (false, true, false) => Some(portal::FILE_TRANSFER),
            (_, _, true) => Some(URI_LIST),
            _ => None,
        }
    }

    const fn over(drag: &Drag) -> DragState {
        DragState::Over(DragOver {
            count: 0,
            position: Some(drag.position),
            window_id: drag.window_id,
        })
    }

    fn enter(
        &mut self,
        serial: u32,
        surface: &WlSurface,
        x: f64,
        y: f64,
        offer: Option<WlDataOffer>,
    ) {
        self.leave();
        let mimes = offer
            .as_ref()
            .and_then(|offer| self.offers.remove(&offer.id()))
            .unwrap_or_default();
        // The app's own drag out, over its own window, is not a drop in.
        let listening =
            self.shared.options().is_some() && !self.shared.dragging_out.load(Ordering::Acquire);
        let mime = if listening { self.pick(&mimes) } else { None };
        if let Some(offer) = &offer {
            offer.accept(serial, mime.map(str::to_owned));
            if offer.version() >= 3 {
                let actions = if mime.is_some() {
                    DndAction::Copy
                } else {
                    DndAction::None
                };
                offer.set_actions(actions, actions);
            }
        }
        let drag = Drag {
            offer,
            mime,
            window_id: self.window_of(surface),
            position: DropPoint {
                x: x as f32,
                y: y as f32,
            },
            last_move: Instant::now(),
        };
        if mime.is_some() {
            self.shared.set_state(Self::over(&drag));
        }
        self.drag = Some(drag);
    }

    fn motion(&mut self, x: f64, y: f64) {
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        drag.position = DropPoint {
            x: x as f32,
            y: y as f32,
        };
        if drag.mime.is_none() || drag.last_move.elapsed() < MOVE_INTERVAL {
            return;
        }
        drag.last_move = Instant::now();
        let state = Self::over(self.drag.as_ref().expect("checked above"));
        self.shared.set_state(state);
    }

    fn leave(&mut self) {
        if let Some(drag) = self.drag.take() {
            if let Some(offer) = drag.offer {
                offer.destroy();
            }
            if drag.mime.is_some() {
                self.shared.set_state(DragState::Idle);
            }
        }
    }

    fn dropped(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        let (Some(offer), Some(mime)) = (drag.offer, drag.mime) else {
            self.shared.set_state(DragState::Idle);
            return;
        };
        let (reader, writer) = match std::io::pipe() {
            Ok(pipe) => pipe,
            Err(err) => {
                tracing::warn!(target: "istmo.dnd.wayland", %err, "pipe");
                offer.destroy();
                self.shared.set_state(DragState::Idle);
                return;
            }
        };
        offer.receive(mime.to_owned(), writer.as_fd());
        // Our end of the writer goes now, or the read never sees EOF.
        drop(writer);
        if let Err(err) = self.conn.flush() {
            tracing::warn!(target: "istmo.dnd.wayland", %err, "flush receive");
        }
        self.shared.set_state(DragState::Receiving(0));
        let shared = Arc::clone(&self.shared);
        let conn = self.conn.clone();
        let spawned = std::thread::Builder::new()
            .name("istmo-dnd-receive".into())
            .spawn(move || {
                let mut payload = Vec::new();
                let read = reader.take(MAX_PAYLOAD).read_to_end(&mut payload);
                if offer.version() >= 3 {
                    offer.finish();
                }
                offer.destroy();
                let _ = conn.flush();
                let result = match read {
                    Ok(_) if mime == portal::FILE_TRANSFER => {
                        portal::retrieve(&payload).map(|paths| (paths, 0))
                    }
                    Ok(_) => Ok(files::parse_uri_list(&String::from_utf8_lossy(&payload))),
                    Err(err) => Err(err.to_string()),
                };
                match result {
                    Ok((paths, others)) => {
                        if let Err(err) =
                            shared.deliver(paths, others, Some(drag.position), drag.window_id)
                        {
                            tracing::warn!(target: "istmo.dnd.wayland", %err, "publish drop");
                        }
                    }
                    Err(err) => tracing::warn!(target: "istmo.dnd.wayland", %err, "read drop"),
                }
                shared.set_state(DragState::Idle);
            });
        if let Err(err) = spawned {
            tracing::warn!(target: "istmo.dnd.wayland", %err, "receive thread");
            self.shared.set_state(DragState::Idle);
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Pump {
    fn event(
        _state: &mut Self,
        _registry: &WlRegistry,
        _event: wayland_client::protocol::wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Pump: ignore WlCallback);
delegate_noop!(Pump: WlDataDeviceManager);
delegate_noop!(Pump: ignore WlSurface);

impl Dispatch<WlDataDevice, ()> for Pump {
    fn event(
        pump: &mut Self,
        _device: &WlDataDevice,
        event: wl_data_device::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        use wl_data_device::Event as E;
        match event {
            E::DataOffer { id } => {
                pump.offers.insert(id.id(), HashSet::new());
            }
            E::Enter {
                serial,
                surface,
                x,
                y,
                id,
            } => pump.enter(serial, &surface, x, y, id),
            E::Motion { x, y, .. } => pump.motion(x, y),
            E::Leave => pump.leave(),
            E::Drop => pump.dropped(),
            // Clipboard offers are the toolkit's business, not ours.
            E::Selection { id: Some(offer) } => {
                pump.offers.remove(&offer.id());
                offer.destroy();
            }
            _ => {}
        }
    }

    // Opcode 0 of `wl_data_device` events is `data_offer`, a new object.
    event_created_child!(Pump, WlDataDevice, [
        0 => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for Pump {
    fn event(
        pump: &mut Self,
        offer: &WlDataOffer,
        event: wl_data_offer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event
            && let Some(mimes) = pump.offers.get_mut(&offer.id())
        {
            mimes.insert(mime_type);
        }
    }
}

/// Text types other apps ask a drag's plain text by.
const TEXT_MIMES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];

/// A drag out of the app: what it offers, and who waits for its end.
#[derive(Debug)]
struct Outgoing {
    uris: Option<String>,
    text: Option<String>,
    files: Vec<std::path::PathBuf>,
    /// The document portal's key for the files, made on the first ask.
    portal_key: Mutex<Option<String>>,
    done: Mutex<Option<DragDone>>,
}

impl Outgoing {
    fn finish(&self, result: Result<DragResult, DndError>) {
        let done = lock(&self.done).take();
        if let Some(done) = done {
            done.finish(result);
        }
    }

    /// Bytes for `mime`, `None` for a type the drag does not offer.
    fn data(&self, mime: &str) -> Option<Vec<u8>> {
        if mime == URI_LIST {
            return self.uris.clone().map(String::into_bytes);
        }
        if mime == portal::FILE_TRANSFER {
            let mut key = lock(&self.portal_key);
            if key.is_none() {
                match portal::start_transfer(&self.files) {
                    Ok(made) => *key = Some(made),
                    Err(err) => {
                        tracing::warn!(target: "istmo.dnd.wayland", %err, "portal transfer");
                        return None;
                    }
                }
            }
            return key.clone().map(String::into_bytes);
        }
        TEXT_MIMES
            .contains(&mime)
            .then(|| self.text.clone().map(String::into_bytes))
            .flatten()
    }
}

impl Dispatch<WlSeat, ()> for Pump {
    fn event(
        pump: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        else {
            return;
        };
        if capabilities.contains(Capability::Pointer) && pump.pointer.is_none() {
            pump.pointer = Some(seat.get_pointer(qh, ()));
        }
        if capabilities.contains(Capability::Touch) && pump.touch.is_none() {
            pump.touch = Some(seat.get_touch(qh, ()));
        }
    }
}

impl Dispatch<WlPointer, ()> for Pump {
    fn event(
        pump: &mut Self,
        _pointer: &WlPointer,
        event: wl_pointer::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let mut press = lock(&pump.press);
        match event {
            wl_pointer::Event::Enter { surface, .. } => press.surface = Some(surface),
            wl_pointer::Event::Button { serial, state, .. } => {
                let down = matches!(state, WEnum::Value(wl_pointer::ButtonState::Pressed));
                press.down = down;
                if down {
                    press.serial = Some(serial);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlTouch, ()> for Pump {
    fn event(
        pump: &mut Self,
        _touch: &WlTouch,
        event: wl_touch::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let mut press = lock(&pump.press);
        match event {
            wl_touch::Event::Down {
                serial, surface, ..
            } => {
                press.serial = Some(serial);
                press.surface = Some(surface);
                press.down = true;
            }
            wl_touch::Event::Up { .. } | wl_touch::Event::Cancel => press.down = false,
            _ => {}
        }
    }
}

impl Dispatch<WlDataSource, Arc<Outgoing>> for Pump {
    fn event(
        _pump: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        outgoing: &Arc<Outgoing>,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                let outgoing = Arc::clone(outgoing);
                // The target reads at its pace: write off the pump.
                let spawned = std::thread::Builder::new()
                    .name("istmo-dnd-send".into())
                    .spawn(move || {
                        use std::io::Write as _;
                        if let Some(bytes) = outgoing.data(&mime_type) {
                            let mut pipe = std::fs::File::from(fd);
                            if let Err(err) = pipe.write_all(&bytes) {
                                tracing::debug!(target: "istmo.dnd.wayland", %err, "send drag data");
                            }
                        }
                    });
                if let Err(err) = spawned {
                    tracing::warn!(target: "istmo.dnd.wayland", %err, "send thread");
                }
            }
            wl_data_source::Event::DndFinished => {
                outgoing.finish(Ok(DragResult::Dropped));
                source.destroy();
            }
            wl_data_source::Event::Cancelled => {
                outgoing.finish(Ok(DragResult::Cancelled));
                source.destroy();
            }
            _ => {}
        }
    }
}
