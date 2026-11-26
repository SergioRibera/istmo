//! `org.kde.StatusNotifierItem`.

use std::sync::Weak;

use istmo_core::NativeHandleId;
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedObjectPath;

use crate::host::{Shared, TrayState};
use crate::{Anchor, Orientation, Pointer, PrimaryAction, Rect, ScrollDelta, TrayEvent};

/// Pixmap sizes offered: the panel picks the one that fits.
const SIZES: [u32; 6] = [16, 22, 24, 32, 48, 64];

/// An ARGB32 pixmap in network byte order, as SNI wants it.
type Pixmap = (i32, i32, Vec<u8>);

pub(super) struct Item {
    shared: Weak<Shared>,
    tray: NativeHandleId,
}

impl Item {
    pub(super) const fn new(shared: Weak<Shared>, tray: NativeHandleId) -> Self {
        Self { shared, tray }
    }

    fn read<T: Default>(&self, read: impl FnOnce(&TrayState) -> T) -> T {
        self.shared
            .upgrade()
            .and_then(|shared| shared.lock().trays.get(&self.tray).map(read))
            .unwrap_or_default()
    }

    fn emit(&self, event: TrayEvent) {
        if let Some(shared) = self.shared.upgrade() {
            shared.emit(self.tray, event);
        }
    }

    /// The text shown next to the icon (`XAyatanaLabel`).
    pub(super) fn label(&self) -> String {
        self.read(|tray| tray.title.clone().unwrap_or_default())
    }

    pub(super) fn status(&self) -> String {
        self.read(|tray| if tray.visible { "Active" } else { "Passive" }.to_owned())
    }

    fn pointer(x: i32, y: i32) -> Pointer {
        let precision = super::anchor_precision();
        Pointer {
            anchor: Some(Anchor {
                rect: Rect {
                    x,
                    y,
                    width: 0,
                    height: 0,
                },
                scale: 1.0,
                precision,
            }),
        }
    }
}

/// The icon's pixmaps, when it is not a theme name.
fn pixmaps(tray: &TrayState) -> Vec<Pixmap> {
    let icon = tray.icon.frame();
    if icon.name().is_some() {
        return Vec::new();
    }
    SIZES
        .iter()
        .filter_map(|&size| icon.raster(size))
        .map(|raster| {
            let argb = raster
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|&[r, g, b, a]| [a, r, g, b])
                .collect();
            (
                i32::try_from(raster.width).unwrap_or(i32::MAX),
                i32::try_from(raster.height).unwrap_or(i32::MAX),
                argb,
            )
        })
        .collect()
}

#[allow(
    clippy::unused_self,
    clippy::missing_const_for_fn,
    clippy::unnecessary_literal_bound,
    clippy::used_underscore_binding,
    clippy::needless_pass_by_value,
    reason = "D-Bus method shapes: `&self` receivers, owned arguments and literal properties the interface macro requires; the macro also expands the `_` bindings"
)]
#[interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }

    #[zbus(property)]
    fn id(&self) -> String {
        self.read(|tray| tray.id.clone())
    }

    #[zbus(property)]
    fn title(&self) -> String {
        self.read(|tray| tray.title.clone().unwrap_or_else(|| tray.id.clone()))
    }

    #[zbus(property, name = "Status")]
    fn status_property(&self) -> String {
        self.status()
    }

    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }

    #[zbus(property)]
    fn icon_name(&self) -> String {
        self.read(|tray| tray.icon.frame().name().unwrap_or_default().to_owned())
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<Pixmap> {
        self.read(pixmaps)
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn overlay_icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn overlay_icon_pixmap(&self) -> Vec<Pixmap> {
        Vec::new()
    }

    #[zbus(property)]
    fn attention_icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Vec<Pixmap> {
        Vec::new()
    }

    #[zbus(property)]
    fn attention_movie_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn tool_tip(&self) -> (String, Vec<Pixmap>, String, String) {
        self.read(|tray| {
            tray.tooltip.as_ref().map_or_else(Default::default, |tip| {
                (
                    String::new(),
                    Vec::new(),
                    tip.title.clone(),
                    tip.body.clone().unwrap_or_default(),
                )
            })
        })
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        self.read(|tray| tray.primary == PrimaryAction::ShowMenu)
    }

    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(super::MENU_PATH)
            .expect("invariant: the menu path is a valid object path")
    }

    #[zbus(property, name = "XAyatanaLabel")]
    fn x_ayatana_label(&self) -> String {
        self.label()
    }

    #[zbus(property, name = "XAyatanaLabelGuide")]
    fn x_ayatana_label_guide(&self) -> &str {
        ""
    }

    fn activate(&self, x: i32, y: i32) {
        let primary = self.read(|tray| Some(tray.primary)).unwrap_or_default();
        // With `ShowMenu` the host opens the menu; an `Activate` from a
        // host that ignores `ItemIsMenu` is not the app's business.
        if primary == PrimaryAction::Activate {
            self.emit(TrayEvent::Activate(Self::pointer(x, y)));
        }
    }

    fn secondary_activate(&self, x: i32, y: i32) {
        self.emit(TrayEvent::SecondaryActivate(Self::pointer(x, y)));
    }

    // The host shows the exported menu itself.
    #[allow(unused_variables, reason = "the D-Bus signature takes the point")]
    fn context_menu(&self, x: i32, y: i32) {}

    fn scroll(&self, delta: i32, orientation: &str) {
        let orientation = if orientation.eq_ignore_ascii_case("horizontal") {
            Orientation::Horizontal
        } else {
            Orientation::Vertical
        };
        self.emit(TrayEvent::Scroll(ScrollDelta { delta, orientation }));
    }

    #[zbus(signal)]
    pub(super) async fn new_title(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    pub(super) async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    pub(super) async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    pub(super) async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;

    #[zbus(signal, name = "XAyatanaNewLabel")]
    pub(super) async fn x_ayatana_new_label(
        emitter: &SignalEmitter<'_>,
        label: &str,
        guide: &str,
    ) -> zbus::Result<()>;
}
