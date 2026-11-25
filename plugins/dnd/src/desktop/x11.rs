//! Drags out of X11 windows (XDND, version 5).
//!
//! The press belongs to the app: its toolkit holds the pointer's implicit
//! grab, which another client cannot take. So the drag runs on a
//! connection of its own without a grab: it reads the pointer
//! (`QueryPointer`) about sixty times a second, finds the window under it
//! that speaks XDND (`XdndAware`), and talks to that window with
//! `XdndEnter` / `XdndPosition` / `XdndLeave` / `XdndDrop`. A hidden
//! window of this connection owns `XdndSelection` and answers the
//! target's requests for the data. Letting go of the button drops; the
//! drag ends with the target's `XdndFinished`, or cancelled when nothing
//! took it.

use std::time::{Duration, Instant};

use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, KeyButMask,
    PropMode, SELECTION_NOTIFY_EVENT, SelectionNotifyEvent, SelectionRequestEvent, Window,
    WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{COPY_DEPTH_FROM_PARENT, COPY_FROM_PARENT, CURRENT_TIME, NONE};

use super::{DragDone, outgoing};
use crate::{DndError, DragRequest, DragResult};

const XDND_VERSION: u32 = 5;
const POLL: Duration = Duration::from_millis(16);
/// How long a target may take to say it finished after the drop.
const FINISH_WAIT: Duration = Duration::from_secs(5);

/// Text types the drag answers with its plain text.
const TEXT_TYPES: [&str; 5] = [
    "UTF8_STRING",
    "text/plain;charset=utf-8",
    "text/plain",
    "STRING",
    "TEXT",
];

pub(super) fn start_drag(request: DragRequest, done: DragDone) {
    let spawned = std::thread::Builder::new()
        .name("istmo-dnd-x11".into())
        .spawn(move || {
            let result = run(&request);
            done.finish(result);
        });
    if let Err(err) = spawned {
        tracing::warn!(target: "istmo.dnd.x11", %err, "drag thread");
    }
}

/// Atoms the drag uses.
struct Atoms {
    aware: Atom,
    selection: Atom,
    enter: Atom,
    position: Atom,
    status: Atom,
    leave: Atom,
    drop: Atom,
    finished: Atom,
    action_copy: Atom,
    type_list: Atom,
    targets: Atom,
    uri_list: Atom,
    text: Vec<Atom>,
}

impl Atoms {
    fn intern(conn: &RustConnection) -> Result<Self, DndError> {
        let atom = |name: &str| -> Result<Atom, DndError> {
            Ok(conn
                .intern_atom(false, name.as_bytes())
                .map_err(backend)?
                .reply()
                .map_err(backend)?
                .atom)
        };
        Ok(Self {
            aware: atom("XdndAware")?,
            selection: atom("XdndSelection")?,
            enter: atom("XdndEnter")?,
            position: atom("XdndPosition")?,
            status: atom("XdndStatus")?,
            leave: atom("XdndLeave")?,
            drop: atom("XdndDrop")?,
            finished: atom("XdndFinished")?,
            action_copy: atom("XdndActionCopy")?,
            type_list: atom("XdndTypeList")?,
            targets: atom("TARGETS")?,
            uri_list: atom("text/uri-list")?,
            text: TEXT_TYPES
                .iter()
                .map(|name| atom(name))
                .collect::<Result<_, _>>()?,
        })
    }
}

fn backend(err: impl std::fmt::Display) -> DndError {
    DndError::Backend(format!("x11: {err}"))
}

/// What the drag hands over, by type.
struct Payload {
    uris: Option<String>,
    text: Option<String>,
    types: Vec<Atom>,
}

impl Payload {
    fn data(&self, atoms: &Atoms, target: Atom) -> Option<Vec<u8>> {
        if target == atoms.uri_list {
            return self.uris.clone().map(String::into_bytes);
        }
        if atoms.text.contains(&target) {
            return self.text.clone().map(String::into_bytes);
        }
        None
    }
}

/// The XDND window the drag is over, with its protocol version.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Target {
    window: Window,
    version: u32,
}

#[allow(clippy::too_many_lines, reason = "one loop drives the whole drag")]
fn run(request: &DragRequest) -> Result<DragResult, DndError> {
    let (conn, screen) = x11rb::connect(None).map_err(backend)?;
    let root = conn.setup().roots[screen].root;
    let atoms = Atoms::intern(&conn)?;

    // The press must still be down: the drag follows it.
    let pointer = conn
        .query_pointer(root)
        .map_err(backend)?
        .reply()
        .map_err(backend)?;
    if !pressed(pointer.mask) {
        return Err(DndError::NotPressed);
    }

    let source = conn.generate_id().map_err(backend)?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        source,
        root,
        -10,
        -10,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        COPY_FROM_PARENT,
        &CreateWindowAux::new(),
    )
    .map_err(backend)?;

    let uris = outgoing::uri_list(&request.items);
    let text = outgoing::plain_text(&request.items);
    let mut types = Vec::new();
    if uris.is_some() {
        types.push(atoms.uri_list);
    }
    if text.is_some() {
        types.extend(&atoms.text);
    }
    let payload = Payload { uris, text, types };
    conn.change_property32(
        PropMode::REPLACE,
        source,
        atoms.type_list,
        AtomEnum::ATOM,
        &payload.types,
    )
    .map_err(backend)?;
    conn.set_selection_owner(source, atoms.selection, CURRENT_TIME)
        .map_err(backend)?;
    conn.flush().map_err(backend)?;

    let mut over: Option<Target> = None;
    let mut accepted = false;
    let mut waiting_status = false;
    let mut last_sent: Option<(i16, i16)> = None;
    let mut dropped_at: Option<Instant> = None;

    let result = 'drag: loop {
        while let Some(event) = conn.poll_for_event().map_err(backend)? {
            match event {
                Event::ClientMessage(message) if message.type_ == atoms.status => {
                    let data = message.data.as_data32();
                    if over.is_some_and(|target| target.window == data[0]) {
                        accepted = data[1] & 1 == 1;
                        waiting_status = false;
                    }
                }
                Event::ClientMessage(message)
                    if message.type_ == atoms.finished && dropped_at.is_some() =>
                {
                    {
                        let data = message.data.as_data32();
                        // Version 5 says whether it worked; earlier ones
                        // only that it is over.
                        let ok = over.is_none_or(|t| t.version < 5) || data[1] & 1 == 1;
                        break 'drag if ok {
                            Ok(DragResult::Dropped)
                        } else {
                            Ok(DragResult::Cancelled)
                        };
                    }
                }
                Event::SelectionRequest(asked) => answer(&conn, &atoms, &payload, &asked)?,
                _ => {}
            }
        }
        if let Some(at) = dropped_at {
            if at.elapsed() > FINISH_WAIT {
                // It took the drop but never said so: count it dropped.
                break Ok(DragResult::Dropped);
            }
            std::thread::sleep(POLL);
            continue;
        }

        let pointer = conn
            .query_pointer(root)
            .map_err(backend)?
            .reply()
            .map_err(backend)?;
        let target = target_under(&conn, &atoms, root)?;
        if target != over {
            if let Some(previous) = over {
                send(
                    &conn,
                    &atoms,
                    previous.window,
                    atoms.leave,
                    [source, 0, 0, 0, 0],
                )?;
            }
            if let Some(next) = target {
                let more = u32::from(payload.types.len() > 3);
                let mut data = [source, (next.version << 24) | more, NONE, NONE, NONE];
                for (slot, atom) in data[2..].iter_mut().zip(&payload.types) {
                    *slot = *atom;
                }
                send(&conn, &atoms, next.window, atoms.enter, data)?;
            }
            over = target;
            accepted = false;
            waiting_status = false;
            last_sent = None;
        }

        if !pressed(pointer.mask) {
            match over {
                Some(target) if accepted => {
                    send(
                        &conn,
                        &atoms,
                        target.window,
                        atoms.drop,
                        [source, 0, CURRENT_TIME, 0, 0],
                    )?;
                    dropped_at = Some(Instant::now());
                    continue;
                }
                Some(target) => {
                    send(
                        &conn,
                        &atoms,
                        target.window,
                        atoms.leave,
                        [source, 0, 0, 0, 0],
                    )?;
                    break Ok(DragResult::Cancelled);
                }
                None => break Ok(DragResult::Cancelled),
            }
        }

        let at = (pointer.root_x, pointer.root_y);
        if let Some(target) = over
            && !waiting_status
            && last_sent != Some(at)
        {
            #[allow(
                clippy::cast_sign_loss,
                reason = "screen coordinates are packed as u16"
            )]
            let packed = (u32::from(at.0 as u16) << 16) | u32::from(at.1 as u16);
            send(
                &conn,
                &atoms,
                target.window,
                atoms.position,
                [source, 0, packed, CURRENT_TIME, atoms.action_copy],
            )?;
            waiting_status = true;
            last_sent = Some(at);
        }
        std::thread::sleep(POLL);
    };

    let _ = conn.destroy_window(source);
    let _ = conn.flush();
    result
}

/// Any mouse button held.
fn pressed(mask: KeyButMask) -> bool {
    mask.intersects(KeyButMask::BUTTON1 | KeyButMask::BUTTON2 | KeyButMask::BUTTON3)
}

/// The window under the pointer that speaks XDND, walking down from the
/// root through the windows the pointer is in.
fn target_under(
    conn: &RustConnection,
    atoms: &Atoms,
    root: Window,
) -> Result<Option<Target>, DndError> {
    let mut window = conn
        .query_pointer(root)
        .map_err(backend)?
        .reply()
        .map_err(backend)?
        .child;
    while window != NONE {
        let aware = conn
            .get_property(false, window, atoms.aware, AtomEnum::ATOM, 0, 1)
            .map_err(backend)?
            .reply()
            .map_err(backend)?;
        if let Some(version) = aware.value32().and_then(|mut values| values.next()) {
            return Ok(Some(Target {
                window,
                version: version.min(XDND_VERSION),
            }));
        }
        window = conn
            .query_pointer(window)
            .map_err(backend)?
            .reply()
            .map_err(backend)?
            .child;
    }
    Ok(None)
}

fn send(
    conn: &RustConnection,
    atoms: &Atoms,
    window: Window,
    kind: Atom,
    data: [u32; 5],
) -> Result<(), DndError> {
    let _ = atoms;
    let message = ClientMessageEvent::new(32, window, kind, data);
    conn.send_event(false, window, EventMask::NO_EVENT, message)
        .map_err(backend)?;
    conn.flush().map_err(backend)
}

/// Hand the target the data it asked for, or tell it there is none.
fn answer(
    conn: &RustConnection,
    atoms: &Atoms,
    payload: &Payload,
    asked: &SelectionRequestEvent,
) -> Result<(), DndError> {
    let property = if asked.property == NONE {
        asked.target
    } else {
        asked.property
    };
    let stored = if asked.target == atoms.targets {
        let mut targets = payload.types.clone();
        targets.push(atoms.targets);
        conn.change_property32(
            PropMode::REPLACE,
            asked.requestor,
            property,
            AtomEnum::ATOM,
            &targets,
        )
        .map_err(backend)?;
        true
    } else if let Some(bytes) = payload.data(atoms, asked.target) {
        conn.change_property8(
            PropMode::REPLACE,
            asked.requestor,
            property,
            asked.target,
            &bytes,
        )
        .map_err(backend)?;
        true
    } else {
        false
    };
    let notify = SelectionNotifyEvent {
        response_type: SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: asked.time,
        requestor: asked.requestor,
        selection: asked.selection,
        target: asked.target,
        property: if stored { property } else { NONE },
    };
    conn.send_event(false, asked.requestor, EventMask::NO_EVENT, notify)
        .map_err(backend)?;
    conn.flush().map_err(backend)
}
