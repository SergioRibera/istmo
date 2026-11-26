//! Linux: `org.kde.StatusNotifierItem` + `com.canonical.dbusmenu` on
//! the session bus.
//!
//! Every tray item gets its own bus connection, so the watcher drops
//! the item the moment its connection closes (it tracks items by bus
//! name). The item asks for the well-known name
//! `org.kde.StatusNotifierItem-<pid>-<n>` and registers it; where the
//! name cannot be owned (a Flatpak sandbox without `--own-name`) it
//! registers its object path, which watchers resolve against the
//! sender.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Weak};

use istmo_core::NativeHandleId;
use zbus::blocking::Connection;
use zbus::fdo::RequestNameFlags;

use crate::host::{Backend, Dirty, MenuDirty, Shared};
use crate::{
    AnchorPrecision, EventSupport, HostState, MenuSupport, Support, TrayBackend, TrayCapabilities,
    TrayError,
};

mod item;
mod menu;

use item::Item;
use menu::DbusMenu;

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";

/// What the bus thread is woken for.
enum Wake {
    Changes,
    /// The watcher (re)appeared: register every item again.
    Watcher,
}

struct Sni {
    wake: flume::Sender<Wake>,
}

impl Backend for Sni {
    fn capabilities(&self) -> TrayCapabilities {
        capabilities()
    }

    fn wake(&self) {
        let _sent = self.wake.send(Wake::Changes);
    }
}

pub(super) fn start(shared: &Arc<Shared>) -> Result<Arc<dyn Backend>, TrayError> {
    let bus = Connection::session().map_err(platform)?;
    shared.set_host(host_state(&bus));
    let (tx, rx) = flume::unbounded();
    let thread_shared = Arc::downgrade(shared);
    std::thread::Builder::new()
        .name("istmo-tray-dbus".to_owned())
        .spawn(move || run(&thread_shared, &rx))
        .map_err(|err| TrayError::Platform(err.to_string()))?;
    watch_watcher(bus, Arc::downgrade(shared), tx.clone());
    Ok(Arc::new(Sni { wake: tx }))
}

#[expect(clippy::needless_pass_by_value, reason = "used as `map_err(platform)`")]
fn platform(err: zbus::Error) -> TrayError {
    TrayError::Platform(err.to_string())
}

/// Whether a watcher with a host is on the bus.
fn host_state(bus: &Connection) -> HostState {
    let present = bus
        .call_method(
            Some(WATCHER),
            WATCHER_PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(WATCHER, "IsStatusNotifierHostRegistered"),
        )
        .ok()
        .and_then(|reply| {
            reply
                .body()
                .deserialize::<zbus::zvariant::OwnedValue>()
                .ok()
        })
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false);
    if present {
        HostState::Available
    } else {
        HostState::Missing
    }
}

/// Follows the watcher's bus name: when it comes back (the panel
/// restarted) every item registers again.
fn watch_watcher(bus: Connection, shared: Weak<Shared>, wake: flume::Sender<Wake>) {
    let spawned = std::thread::Builder::new()
        .name("istmo-tray-watcher".to_owned())
        .spawn(move || {
            let Ok(proxy) = zbus::blocking::fdo::DBusProxy::new(&bus) else {
                return;
            };
            let Ok(changes) = proxy.receive_name_owner_changed_with_args(&[(0, WATCHER)]) else {
                return;
            };
            for change in changes {
                let Some(shared) = shared.upgrade() else {
                    return;
                };
                let Ok(args) = change.args() else {
                    continue;
                };
                if args.new_owner().is_some() {
                    shared.set_host(HostState::Restarted);
                    shared.set_host(host_state(&bus));
                    if wake.send(Wake::Watcher).is_err() {
                        return;
                    }
                } else {
                    shared.set_host(HostState::Missing);
                }
            }
        });
    if let Err(err) = spawned {
        tracing::warn!(%err, "tray: cannot follow the StatusNotifierWatcher");
    }
}

/// One item's connection and the bus name it registered.
struct ItemBus {
    conn: Connection,
    registered: String,
    revision: Arc<AtomicU32>,
}

/// The bus thread: mirrors pending changes onto the items' objects.
fn run(shared: &Weak<Shared>, wake: &flume::Receiver<Wake>) {
    let mut items: HashMap<NativeHandleId, ItemBus> = HashMap::new();
    while let Ok(reason) = wake.recv() {
        let Some(shared) = shared.upgrade() else {
            return;
        };
        // Everything queued so far is handled by one pass.
        let mut reregister = matches!(reason, Wake::Watcher);
        reregister |= wake.drain().any(|w| matches!(w, Wake::Watcher));
        for (tray, dirty) in shared.take_pending() {
            apply(&shared, &mut items, tray, &dirty);
        }
        if reregister {
            for bus in items.values() {
                register(bus);
            }
        }
    }
}

fn apply(
    shared: &Arc<Shared>,
    items: &mut HashMap<NativeHandleId, ItemBus>,
    tray: NativeHandleId,
    dirty: &Dirty,
) {
    if dirty.removed {
        // Closing the connection unregisters the item.
        items.remove(&tray);
        if !dirty.added {
            return;
        }
    }
    if dirty.added {
        items.remove(&tray);
        match open_item(shared, tray) {
            Ok(Some(bus)) => {
                items.insert(tray, bus);
            }
            Ok(None) => {}
            Err(err) => tracing::warn!(%err, "tray: cannot publish a StatusNotifierItem"),
        }
        return;
    }
    let Some(bus) = items.get(&tray) else {
        return;
    };
    if let Err(err) = emit_changes(shared, bus, tray, dirty) {
        tracing::debug!(%err, "tray: change signal failed");
    }
}

fn open_item(shared: &Arc<Shared>, tray: NativeHandleId) -> zbus::Result<Option<ItemBus>> {
    let Some(serial) = shared.lock().trays.get(&tray).map(|t| t.serial) else {
        return Ok(None);
    };
    let revision = Arc::new(AtomicU32::new(1));
    let conn = zbus::blocking::connection::Builder::session()?
        .serve_at(ITEM_PATH, Item::new(Arc::downgrade(shared), tray))?
        .serve_at(
            MENU_PATH,
            DbusMenu::new(Arc::downgrade(shared), tray, Arc::clone(&revision)),
        )?
        .build()?;
    let name = format!("org.kde.StatusNotifierItem-{}-{serial}", std::process::id());
    let owned = conn
        .request_name_with_flags(name.as_str(), RequestNameFlags::DoNotQueue.into())
        .is_ok_and(|reply| reply == zbus::fdo::RequestNameReply::PrimaryOwner);
    let registered = if owned { name } else { ITEM_PATH.to_owned() };
    let bus = ItemBus {
        conn,
        registered,
        revision,
    };
    register(&bus);
    Ok(Some(bus))
}

fn register(bus: &ItemBus) {
    let result = bus.conn.call_method(
        Some(WATCHER),
        WATCHER_PATH,
        Some(WATCHER),
        "RegisterStatusNotifierItem",
        &(bus.registered.as_str(),),
    );
    if let Err(err) = result {
        // No watcher yet: the item registers when one appears.
        tracing::debug!(%err, "tray: StatusNotifierWatcher registration failed");
    }
}

fn emit_changes(
    shared: &Arc<Shared>,
    bus: &ItemBus,
    tray: NativeHandleId,
    dirty: &Dirty,
) -> zbus::Result<()> {
    let objects = bus.conn.object_server();
    if dirty.icon || dirty.title || dirty.tooltip || dirty.visible {
        let item = objects.interface::<_, Item>(ITEM_PATH)?;
        let emitter = item.signal_emitter();
        if dirty.icon {
            zbus::block_on(Item::new_icon(emitter))?;
        }
        if dirty.title {
            zbus::block_on(Item::new_title(emitter))?;
            let label = item.get().label();
            zbus::block_on(Item::x_ayatana_new_label(emitter, &label, ""))?;
        }
        if dirty.tooltip {
            zbus::block_on(Item::new_tool_tip(emitter))?;
        }
        if dirty.visible {
            let status = item.get().status();
            zbus::block_on(Item::new_status(emitter, &status))?;
        }
    }
    match &dirty.menu {
        MenuDirty::Clean => {}
        MenuDirty::Patches(patches) => {
            let menu = objects.interface::<_, DbusMenu>(MENU_PATH)?;
            let (updated, removed) = menu::properties_update(shared, tray, patches);
            zbus::block_on(DbusMenu::items_properties_updated(
                menu.signal_emitter(),
                updated,
                removed,
            ))?;
        }
        MenuDirty::Rebuild => {
            let menu = objects.interface::<_, DbusMenu>(MENU_PATH)?;
            let revision = bus.revision.fetch_add(1, Ordering::Relaxed) + 1;
            zbus::block_on(DbusMenu::layout_updated(menu.signal_emitter(), revision, 0))?;
        }
    }
    Ok(())
}

/// Wayland panels pass coordinates of their own choosing, often 0, 0:
/// the point says nothing about where the icon is.
pub(super) fn anchor_precision() -> AnchorPrecision {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        AnchorPrecision::Unreliable
    } else {
        AnchorPrecision::ClickPoint
    }
}

fn capabilities() -> TrayCapabilities {
    TrayCapabilities {
        backend: TrayBackend::Sni,
        host: HostState::Available,
        max_items: None,
        min_frame_ms: 33,
        image_names: Support::Native,
        template_images: Support::Unsupported,
        title: Support::HostDependent,
        tooltip_body: Support::Native,
        menu: MenuSupport {
            check: Support::Native,
            mixed: Support::HostDependent,
            radio: Support::HostDependent,
            item_icons: Support::Native,
            shortcuts: Support::Native,
            header: Support::Emulated,
            subtitle: Support::Unsupported,
            badge: Support::Unsupported,
            default_item: Support::Unsupported,
            mnemonics: Support::Native,
            live_while_open: Support::HostDependent,
        },
        events: EventSupport {
            activate: Support::Native,
            secondary: Support::Native,
            double_click: Support::Unsupported,
            scroll: Support::Native,
            menu_open_close: Support::HostDependent,
        },
        anchor: Some(anchor_precision()),
    }
}
