//! `com.canonical.dbusmenu` (version 3).
//!
//! Item ids are the app's `ItemId` + 1 (0 is the root). Ids that do not
//! fit an `i32` after that are left out of the exported menu.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Weak};

use istmo_core::NativeHandleId;
use istmo_menu::{CheckState, ItemChange, ItemId, ItemKind, ItemPatch, Label, MenuNode, Modifiers};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedValue, Value};

use crate::TrayEvent;
use crate::host::{Shared, TrayState};

/// Item icon size sent to the host.
const ICON_SIZE: u32 = 24;

type Properties = HashMap<String, OwnedValue>;
/// `(ia{sv}av)`: id, properties, children (each a variant holding the
/// same structure).
type Layout = (i32, Properties, Vec<OwnedValue>);
/// One item's changed properties.
pub(super) type UpdatedProperties = (i32, Properties);
/// One item's properties back to their defaults.
pub(super) type RemovedProperties = (i32, Vec<String>);

fn owned<'a>(value: impl Into<Value<'a>>) -> OwnedValue {
    value
        .into()
        .try_into_owned()
        .expect("invariant: menu properties carry no file descriptors")
}

fn dbus_id(id: ItemId) -> Option<i32> {
    i32::try_from(id.value).ok()?.checked_add(1)
}

fn item_id(id: i32) -> Option<ItemId> {
    u32::try_from(id.checked_sub(1)?).ok().map(ItemId::from)
}

fn shortcut_keys(modifiers: Modifiers, key: &str) -> Vec<Vec<String>> {
    let mut keys = Vec::with_capacity(5);
    for (bit, name) in [
        (Modifiers::CONTROL, "Control"),
        (Modifiers::ALT, "Alt"),
        (Modifiers::SHIFT, "Shift"),
        (Modifiers::SUPER, "Super"),
    ] {
        if modifiers.contains(bit) {
            keys.push(name.to_owned());
        }
    }
    keys.push(key.to_owned());
    vec![keys]
}

/// Every property of `node` (defaults left out, as the spec allows).
fn properties(tray: &TrayState, node: &MenuNode) -> Properties {
    let mut props = Properties::new();
    let mut put = |name: &str, value: OwnedValue| {
        props.insert(name.to_owned(), value);
    };
    if matches!(node.kind, ItemKind::Separator) {
        put("type", owned("separator"));
    } else {
        put("label", owned(Label::parse(&node.label).with_marker('_')));
    }
    let clickable = !matches!(node.kind, ItemKind::Header | ItemKind::Separator);
    if !node.enabled || !clickable {
        put("enabled", owned(false));
    }
    if !node.visible {
        put("visible", owned(false));
    }
    match node.kind {
        ItemKind::Check(check) => {
            put("toggle-type", owned("checkmark"));
            put("toggle-state", owned(toggle_state(check.state)));
        }
        ItemKind::Radio(radio) => {
            put("toggle-type", owned("radio"));
            put("toggle-state", owned(i32::from(radio.selected)));
        }
        ItemKind::Submenu => put("children-display", owned("submenu")),
        // Actions, separators, headers, and kinds added to the model later.
        _ => {}
    }
    if let Some(shortcut) = &node.shortcut {
        put(
            "shortcut",
            owned(shortcut_keys(shortcut.modifiers, &shortcut.key)),
        );
    }
    if let Some(icon) = tray.item_icons.get(&node.id) {
        if let Some(name) = icon.name() {
            put("icon-name", owned(name.to_owned()));
        } else if let Some(raster) = icon.raster(ICON_SIZE) {
            put("icon-data", owned(raster.to_png()));
        }
    }
    props
}

const fn toggle_state(state: CheckState) -> i32 {
    match state {
        CheckState::Off => 0,
        CheckState::On => 1,
        // `Mixed` and any state added later.
        _ => -1,
    }
}

fn filter(mut props: Properties, names: &[String]) -> Properties {
    if !names.is_empty() {
        props.retain(|name, _| names.contains(name));
    }
    props
}

/// The layout under `parent`, `depth` levels deep (`-1`: all).
fn layout(
    tray: &TrayState,
    parent: Option<ItemId>,
    depth: i32,
    names: &[String],
) -> Vec<OwnedValue> {
    if depth == 0 {
        return Vec::new();
    }
    tray.menu
        .children(parent)
        .filter_map(|node| {
            let id = dbus_id(node.id)?;
            let children = if matches!(node.kind, ItemKind::Submenu) {
                layout(tray, Some(node.id), depth - 1, names)
            } else {
                Vec::new()
            };
            Some(owned((id, filter(properties(tray, node), names), children)))
        })
        .collect()
}

fn root_properties() -> Properties {
    HashMap::from([("children-display".to_owned(), owned("submenu"))])
}

/// The `ItemsPropertiesUpdated` arguments for applied patches: every
/// property of each patched item, plus the icon properties of items
/// whose icon was removed.
pub(super) fn properties_update(
    shared: &Arc<Shared>,
    tray: NativeHandleId,
    patches: &[ItemPatch],
) -> (Vec<UpdatedProperties>, Vec<RemovedProperties>) {
    let mut ids: Vec<ItemId> = patches.iter().map(|p| p.id).collect();
    ids.sort_unstable();
    ids.dedup();
    let updated = {
        let state = shared.lock();
        let Some(entry) = state.trays.get(&tray) else {
            return (Vec::new(), Vec::new());
        };
        let updated = ids
            .iter()
            .filter_map(|&id| Some((dbus_id(id)?, properties(entry, entry.menu.get(id)?))))
            .collect();
        drop(state);
        updated
    };
    let mut removed = Vec::new();
    for patch in patches {
        let Some(id) = dbus_id(patch.id) else {
            continue;
        };
        let names: &[&str] = match &patch.change {
            ItemChange::Icon(None) => &["icon-name", "icon-data"],
            ItemChange::Shortcut(None) => &["shortcut"],
            ItemChange::Enabled(true) => &["enabled"],
            ItemChange::Visible(true) => &["visible"],
            _ => &[],
        };
        if !names.is_empty() {
            removed.push((id, names.iter().map(|&n| n.to_owned()).collect()));
        }
    }
    (updated, removed)
}

pub(super) struct DbusMenu {
    shared: Weak<Shared>,
    tray: NativeHandleId,
    revision: Arc<AtomicU32>,
}

impl DbusMenu {
    pub(super) const fn new(
        shared: Weak<Shared>,
        tray: NativeHandleId,
        revision: Arc<AtomicU32>,
    ) -> Self {
        Self {
            shared,
            tray,
            revision,
        }
    }

    fn read<T: Default>(&self, read: impl FnOnce(&TrayState) -> T) -> T {
        self.shared
            .upgrade()
            .and_then(|shared| shared.lock().trays.get(&self.tray).map(read))
            .unwrap_or_default()
    }

    fn dispatch(&self, id: i32, event_id: &str) {
        let Some(shared) = self.shared.upgrade() else {
            return;
        };
        match (event_id, id) {
            ("clicked", _) => {
                if let Some(item) = item_id(id) {
                    shared.activate_item(self.tray, item);
                }
            }
            ("opened", 0) => shared.emit(self.tray, TrayEvent::MenuOpened),
            ("closed", 0) => shared.emit(self.tray, TrayEvent::MenuClosed),
            _ => {}
        }
    }
}

#[allow(
    clippy::unused_self,
    clippy::missing_const_for_fn,
    clippy::unnecessary_literal_bound,
    clippy::used_underscore_binding,
    clippy::needless_pass_by_value,
    reason = "D-Bus method shapes: `&self` receivers, owned arguments and literal properties the interface macro requires; the macro also expands the `_` bindings"
)]
#[interface(name = "com.canonical.dbusmenu")]
impl DbusMenu {
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: Vec<String>,
    ) -> (u32, Layout) {
        let revision = self.revision.load(Ordering::Relaxed);
        let parent = (parent_id != 0).then(|| item_id(parent_id)).flatten();
        let (props, children) = self.read(|tray| {
            let props = parent.map_or_else(root_properties, |id| {
                tray.menu
                    .get(id)
                    .map(|node| properties(tray, node))
                    .unwrap_or_default()
            });
            (
                filter(props, &property_names),
                layout(tray, parent, recursion_depth, &property_names),
            )
        });
        (revision, (parent_id, props, children))
    }

    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> Vec<(i32, Properties)> {
        self.read(|tray| {
            ids.iter()
                .filter_map(|&id| {
                    let props = if id == 0 {
                        root_properties()
                    } else {
                        properties(tray, tray.menu.get(item_id(id)?)?)
                    };
                    Some((id, filter(props, &property_names)))
                })
                .collect()
        })
    }

    fn get_property(&self, id: i32, name: String) -> OwnedValue {
        let props = self.get_group_properties(vec![id], vec![name.clone()]);
        props
            .into_iter()
            .next()
            .and_then(|(_, mut props)| props.remove(&name))
            .unwrap_or_else(|| owned(""))
    }

    #[allow(
        unused_variables,
        reason = "the D-Bus signature carries data and a timestamp"
    )]
    fn event(&self, id: i32, event_id: &str, data: Value<'_>, timestamp: u32) {
        self.dispatch(id, event_id);
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        for (id, event_id, _, _) in &events {
            self.dispatch(*id, event_id);
        }
        Vec::new()
    }

    // The menu is always current: nothing to refresh before showing.
    #[allow(unused_variables, reason = "the D-Bus signature takes the id")]
    fn about_to_show(&self, id: i32) -> bool {
        false
    }

    #[allow(unused_variables, reason = "the D-Bus signature takes the ids")]
    fn about_to_show_group(&self, ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(signal)]
    pub(super) async fn items_properties_updated(
        emitter: &SignalEmitter<'_>,
        updated_props: Vec<(i32, Properties)>,
        removed_props: Vec<(i32, Vec<String>)>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    pub(super) async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn item_activation_requested(
        emitter: &SignalEmitter<'_>,
        id: i32,
        timestamp: u32,
    ) -> zbus::Result<()>;
}
