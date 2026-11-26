//! Tray icons for [`istmo`](https://docs.rs/istmo): a status item with a
//! menu and a static or animated icon.
//!
//! * Linux — `StatusNotifierItem` + `com.canonical.dbusmenu` over D-Bus
//!   (KDE Plasma, waybar, xfce4-panel, GNOME with the `AppIndicator`
//!   extension, …).
//! * Windows — the notification area (`Shell_NotifyIconW`).
//! * macOS — the menu bar (`NSStatusItem`).
//! * Web — the tab's favicon; only the icon is supported.
//! * Android / iOS — [`UnavailableTray`] answers
//!   [`TrayError::Unsupported`].
//!
//! The plugin never draws: icons are SVG, PNG or JPEG files (or platform
//! icon names) supplied by the app, and swapped by the app at run time.
//! Icons used often are decoded once with
//! [`register_icon`](Tray::register_icon); an [`Animation`] is one call
//! and runs on the plugin's timer.
//!
//! Menus are [`istmo_menu`] node lists. Item kinds a platform lacks are
//! emulated (radio items on macOS, mixed checks on Windows), and
//! [`capabilities`](Tray::capabilities) tells which features are native,
//! emulated, left to the tray host or ignored. Unsupported properties in
//! a menu or spec are ignored rather than refused, so one menu works on
//! every platform.
//!
//! The app registers the host for its platform: [`DesktopTray`] on
//! Linux, Windows and macOS, `WebTray` on the web, [`UnavailableTray`]
//! elsewhere.

#![doc(html_root_url = "https://docs.rs/istmo-tray")]

#[cfg(any(target_os = "linux", target_os = "windows"))]
mod desktop;
#[cfg(any(target_os = "linux", target_os = "windows"))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub(crate)` marks crate-internal items; plain `pub` in private modules trips `unreachable_pub`"
)]
mod host;
#[cfg(not(target_arch = "wasm32"))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub(crate)` marks crate-internal items; plain `pub` in private modules trips `unreachable_pub`"
)]
mod image;
mod types;
mod unavailable;

use istmo_core::{HostCtx, NativeHandleId};
use istmo_macros::{owned, plugin, stream};
use istmo_menu::{IconImage, ItemPatch, MenuNode};

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub use desktop::DesktopTray;
pub use istmo_menu;
pub use types::*;
pub use unavailable::UnavailableTray;

/// Wire identifier of the plugin.
pub const TRAY_PLUGIN_ID: &str = "istmo.tray";

/// The tray contract.
///
/// A tray item is created once and changed in place; every change
/// returns once it is accepted (validated, decoded and queued), not once
/// the tray host has drawn it. Changes made faster than the platform
/// thread applies them collapse to the latest.
#[plugin(name = "istmo.tray", remote = false, crate = "::istmo_core")]
pub trait Tray {
    /// What this backend does natively, emulates or ignores, and
    /// whether a tray host is present right now.
    async fn capabilities(&self) -> TrayCapabilities;

    /// Decodes an icon once, for cheap swaps by handle.
    #[owned]
    async fn register_icon(
        &self,
        ctx: HostCtx,
        image: IconImage,
    ) -> Result<RegisteredIcon, TrayError>;

    /// Adds a tray item. Dropping the owned handle removes it.
    #[owned]
    async fn create(&self, ctx: HostCtx, spec: TraySpec) -> Result<TrayItem, TrayError>;

    /// Replaces the icon (stops a running animation).
    async fn set_icon(&self, tray: NativeHandleId, icon: TrayIcon) -> Result<(), TrayError>;

    /// Text next to the icon, where the platform shows one.
    async fn set_title(&self, tray: NativeHandleId, title: Option<String>)
    -> Result<(), TrayError>;

    async fn set_tooltip(
        &self,
        tray: NativeHandleId,
        tooltip: Option<Tooltip>,
    ) -> Result<(), TrayError>;

    async fn set_visible(&self, tray: NativeHandleId, visible: bool) -> Result<(), TrayError>;

    /// Replaces the menu. A menu with the same items in the same places
    /// is applied as property changes.
    async fn set_menu(&self, tray: NativeHandleId, menu: Vec<MenuNode>) -> Result<(), TrayError>;

    /// Changes single items without resending the menu. All or none.
    async fn update_items(
        &self,
        tray: NativeHandleId,
        patches: Vec<ItemPatch>,
    ) -> Result<(), TrayError>;

    /// Removes the item now (dropping the owned handle does the same).
    async fn destroy(&self, tray: NativeHandleId) -> Result<(), TrayError>;

    /// The item's events. Events emitted while nobody listens are kept
    /// (the last 64) and replayed to the first subscriber.
    #[stream]
    fn events(&self, tray: NativeHandleId) -> Result<TrayEvent, TrayError>;
}
