# `istmo-tray`

Design for a desktop tray plugin: a status icon (Windows notification
area, macOS menu bar, Linux StatusNotifierItem) with a menu, a static or
animated icon that the app swaps at run time, and an event stream. The
menu model lives in a shared crate (`istmo-menu`) so the later context
menu and menu bar work (macOS / Windows) reuses it.

Nothing is implemented yet.

## Scope

v1:
- Linux (StatusNotifierItem + `com.canonical.dbusmenu` over zbus), Windows
  (`Shell_NotifyIconW` + `HMENU`), macOS (`NSStatusItem` + `NSMenu`).
- Web: the icon only (favicon). Every other method answers `Unsupported`.
- Android / iOS: no backend. An `UnavailableTray` host answers
  `Unsupported`, like the other `Unavailable*` hosts.
- All Rust. No Kotlin or Swift sources, no native codegen.

Non-goals:
- Drawing. The plugin never renders icons. It only decodes and converts
  formats (SVG rasterised at the size the host asks for, PNG / JPEG
  decoded, re-encoded to PNG where a protocol demands PNG). Text in an
  icon, progress rings and the like are the app's images.
- Notifications / balloons (a notifications plugin's job).
- Jump Lists, the Dock menu, `.desktop` actions: they work differently
  (static entries, or launch a process).
- The XEmbed tray (`_NET_SYSTEM_TRAY_S0`): legacy X11, and every X11
  panel still in use speaks SNI.

Deferred (sections at the end): popover surfaces, context menu and menu
bar on macOS / Windows, lazily filled submenus.

Ruled out: a context menu on Linux. Outside a toolkit nothing draws a
popup menu for an arbitrary window (a Wayland `xdg_popup` belongs to
the app's own surface and connection), and drawing one ourselves makes
istmo a UI toolkit.

## Decisions taken in discussion

- Desktop only; web keeps the changing icon only.
- Icons are SVG / PNG / JPEG supplied by the app and swapped by hand.
  No `tiny-skia` rendering API. Two icon kinds: static and animated
  (frame list run by the plugin).
- Linux talks to D-Bus through zbus directly, not `ksni`.
- Full menu item coverage on every platform: where a platform lacks a
  kind, the plugin emulates it and the capability report says
  `Emulated`.
- Fresh implementation: no code is copied from previous tray libraries.

## Crate layout

```
crates/menu/                      crate `istmo-menu`: menu model, validation, diff
  build.rs                        istmo_build::emit_shared_types("IstmoMenu", "dev.istmo.menu")
  src/lib.rs                      MenuNode, ItemKind, CheckState, Shortcut, ItemPatch, IconRef / IconImage
  src/validate.rs                 ids, parents, radio groups, bounds
  src/diff.rs                     old/new -> property patches or structural change
plugins/tray/                     crate `istmo-tray`, id `istmo.tray`
  istmo.toml                      manifest (desktop + web targets, see "Build integration")
  src/lib.rs                      trait, owned handles, client sugar
  src/types.rs                    wire types (icons, spec, events, caps, errors)
  src/image.rs                    decode / rasterise / per-size cache (no drawing)
  src/host.rs                     generic host: handle table, event hubs, coalescing
  src/desktop/linux/{sni,dbusmenu,watcher,pixmap}.rs
  src/desktop/windows/{notify,menu,icon,thread}.rs
  src/desktop/macos/{status_item,menu,image,target}.rs
  src/web.rs                      favicon host (wasm32)
  src/unavailable.rs              Android / iOS
  tests/
```

`istmo-menu` is a separate crate even though v1 has a single user,
because a type that crosses crates has to be a shared type group to pass
contract extraction (same mechanism as `istmo-media`). Context menus and
the menu bar will be a second plugin on the same model.

Features: `linux`, `windows`, `macos`, `web` (each default, gated by
target), `svg` (default, pulls `resvg`), `jpeg` (default, `zune-jpeg`).
PNG (`png` crate) is always on: dbusmenu item icons must be PNG bytes.
A build without `svg` answers `TrayError::Image(FormatNotCompiled)` for
SVG input.

## Wire shape constraints

The contract must pass `#[plugin]` portable-shape validation even though
no native code is generated (keeps the door open, costs nothing):
- No recursive types. A recursive `Vec<MenuItem>` submenu would not
  derive (`TYPE_META` cycle), so the menu is a flat list of nodes with
  `parent` links.
- No named-field enum variants, tuples, maps, `Duration`, `usize`,
  `u128`. Durations are `u32` milliseconds.
- No variant named after its payload type (`Scroll(ScrollDelta)`, not
  `Scroll(Scroll)`).
- Public enums `#[non_exhaustive]`.

## Menu model (`istmo-menu`)

```rust
#[istmo::message]
pub struct ItemId(pub u32);

#[istmo::message]
pub struct RadioGroupId(pub u32);

/// One node of a menu. Nodes are listed in display order; a node with
/// `parent: Some(p)` belongs to the submenu `p`, which must come
/// earlier in the list and have `kind: Submenu`.
#[istmo::message]
pub struct MenuNode {
    pub id: ItemId,
    pub parent: Option<ItemId>,
    pub kind: ItemKind,
    pub label: String,               // `&` marks the mnemonic, `&&` a literal `&`
    pub enabled: bool,
    pub visible: bool,
    pub icon: Option<IconRef>,       // see "Icons"
    pub shortcut: Option<Shortcut>,  // shown only, see "Shortcuts"
    pub subtitle: Option<String>,
    pub badge: Option<String>,
    pub is_default: bool,            // Windows bold item, run on double click
}

#[istmo::message]
#[non_exhaustive]
pub enum ItemKind {
    Action,
    Check(CheckItem),
    Radio(RadioItem),
    Submenu,
    Separator,
    Header,                          // section title, not clickable
}

#[istmo::message]
pub struct CheckItem { pub state: CheckState, pub auto_toggle: bool }

#[istmo::message]
#[non_exhaustive]
pub enum CheckState { Off, On, Mixed }

#[istmo::message]
pub struct RadioItem { pub group: RadioGroupId, pub selected: bool, pub auto_select: bool }

#[istmo::message]
pub struct Shortcut { pub modifiers: Modifiers, pub key: String }

#[istmo::message]
pub struct Modifiers(pub u8);        // SHIFT | CONTROL | ALT | SUPER, consts on the type

#[istmo::message]
pub struct ItemPatch { pub id: ItemId, pub change: ItemChange }

#[istmo::message]
#[non_exhaustive]
pub enum ItemChange {
    Label(String),
    Enabled(bool),
    Visible(bool),
    Check(CheckState),
    Selected(bool),                  // radio; the plugin clears the rest of the group
    Icon(Option<IconRef>),
    Subtitle(Option<String>),
    Badge(Option<String>),
    Shortcut(Option<Shortcut>),
}
```

`IconRef` and the image types (see "Icons") live in `istmo-menu` too,
since item icons need them and context menus will as well. The tray
plugin re-exports them.

Validation (`validate.rs`, runs in the host before anything touches the
platform, error names the node):
- ids unique; a parent exists, comes earlier and is a `Submenu`;
- `Separator` / `Header` / `Submenu` carry no check or radio state;
- a radio group lives under one parent and has at most one `selected`;
- bounds: ≤ 4096 nodes, depth ≤ 16, label ≤ 1024 bytes.

`auto_toggle` / `auto_select` (default true in the builders): the plugin
flips a check, or selects a radio and clears the rest of its group, on
the platform thread before emitting the event, so the menu never shows a
stale state while the app's handler runs. With them off, the app owns the
state and answers each event with a patch.

Radio exclusivity is always enforced by the plugin. No platform enforces
it in a tray menu (Win32 `CheckMenuRadioItem` only acts when called).

Diff (`diff.rs`): `set_menu` with the same ids, parents and order as the
current menu becomes a list of property patches; anything else is a
structural change. Patches map to dbusmenu `ItemsPropertiesUpdated` and
in-place `NSMenuItem` mutation; structural changes to `LayoutUpdated`
with a new revision and a rebuild of the affected `NSMenu`. Windows
rebuilds the `HMENU` on every open, so it only stores the new state.

## Icons

```rust
#[istmo::message]
#[non_exhaustive]
pub enum ImageData {
    Svg(Vec<u8>),
    Png(Vec<u8>),
    Jpeg(Vec<u8>),                   // no alpha: draws as an opaque square
    PlatformName(String),            // SF Symbol on macOS, icon-theme name on Linux
}

#[istmo::message]
pub struct IconImage {
    /// Several representations of one picture (e.g. 16 and 32 px PNGs).
    /// The plugin picks per size: SVG first, else the smallest raster
    /// at least as large as the target, else the largest.
    pub reps: Vec<ImageData>,
    /// macOS template image (tinted by the system for light / dark and
    /// highlight). Ignored elsewhere.
    pub template: bool,
}

#[istmo::message]
#[non_exhaustive]
pub enum IconRef {
    Inline(IconImage),
    Registered(NativeHandleId),      // from `register_icon`
}

#[istmo::message]
#[non_exhaustive]
pub enum TrayIcon {
    Static(IconRef),
    Animated(Animation),
}

#[istmo::message]
pub struct Animation { pub frames: Vec<AnimationFrame>, pub repeat: Repeat }

#[istmo::message]
pub struct AnimationFrame { pub icon: IconRef, pub duration_ms: u32 }

#[istmo::message]
#[non_exhaustive]
pub enum Repeat { Forever, Times(u32) }   // after the last run the last frame stays
```

Real time without per-frame traffic:
- `register_icon(IconImage) -> RegisteredIcon` decodes once and caches
  every size the current backend needs (HiDPI included). Swapping to a
  registered icon is an id lookup on the platform thread; dropping the
  owned handle frees the cache entry (`install_native_handle_release_hook`,
  ids from `alloc_native_handle_id`). An app that cycles through ten
  pre-made icons pays the decode ten times in total.
- An animation is one call; the plugin's timer flips frames. Inline
  frames are decoded once when the animation is set.
- Decoded sizes are invalidated on DPI / scale change (`WM_DPICHANGED`,
  `NSScreen` backing change, a host asking a new size) and rebuilt from
  the kept source bytes.

`PlatformName` where unsupported (Windows, web) is an error at
`register_icon` / `set_icon` time, never a silent blank. Apps list a
raster rep after a name for portability: a name rep is tried first only
where names are supported, otherwise skipped.

Bounds: ≤ 8 MiB encoded per rep, decoded ≤ 1024 × 1024, ≤ 512
animation frames, frame duration ≥ the backend's `min_frame_ms`
(clamped, reported in capabilities).

## Control plane

```rust
#[istmo::plugin(name = "istmo.tray", remote = false)]
pub trait Tray {
    /// What this backend does natively, emulates or cannot do, and
    /// whether a tray host is present right now.
    async fn capabilities(&self) -> TrayCapabilities;

    /// Decode and cache an icon for cheap swaps.
    #[owned]
    async fn register_icon(&self, image: IconImage) -> Result<RegisteredIcon, TrayError>;

    /// Add a status item. Dropping the owned handle removes it.
    #[owned]
    async fn create(&self, spec: TraySpec) -> Result<TrayItem, TrayError>;

    async fn set_icon(&self, tray: NativeHandleId, icon: TrayIcon) -> Result<(), TrayError>;
    async fn set_title(&self, tray: NativeHandleId, title: Option<String>) -> Result<(), TrayError>;
    async fn set_tooltip(&self, tray: NativeHandleId, tooltip: Option<Tooltip>) -> Result<(), TrayError>;
    async fn set_visible(&self, tray: NativeHandleId, visible: bool) -> Result<(), TrayError>;

    /// Replace the menu (diffed against the current one).
    async fn set_menu(&self, tray: NativeHandleId, menu: Vec<MenuNode>) -> Result<(), TrayError>;

    /// Change single items without resending the menu.
    async fn update_items(&self, tray: NativeHandleId, patches: Vec<ItemPatch>) -> Result<(), TrayError>;

    async fn destroy(&self, tray: NativeHandleId) -> Result<(), TrayError>;

    #[stream]
    fn events(&self, tray: NativeHandleId) -> Result<TrayEvent, TrayError>;
}

#[istmo::message]
pub struct TraySpec {
    pub id: String,                  // stable per app; SNI `Id`, macOS autosave name
    pub icon: TrayIcon,
    pub title: Option<String>,
    pub tooltip: Option<Tooltip>,
    pub menu: Vec<MenuNode>,
    pub primary: PrimaryAction,
    pub windows_guid: Option<String>,   // see Windows backend
}

#[istmo::message]
#[non_exhaustive]
pub enum PrimaryAction {
    ShowMenu,                        // left click opens the menu
    Activate,                        // left click is an event; right click opens the menu
}

#[istmo::message]
pub struct Tooltip { pub title: String, pub body: Option<String> }
```

`remote = false`: a tray is a property of the GUI process; there is no
desktop peer to serve.

Executor contract: no method is `#[istmo::blocking]`. Each one validates
and decodes on the caller's thread, then hands a command to the backend's
platform thread and awaits a oneshot. Methods run inline through the
typed shortcut, so a `set_icon` costs a lookup, a queue push and a wake.

Coalescing: commands for one tray queue in call order. The platform
thread applies everything queued per wake; consecutive `set_icon` /
`set_title` / `set_tooltip` collapse to the last one, patches are
applied in order. A caller swapping the icon at 1 kHz does not make the
host redraw at 1 kHz.

`set_icon` returns once the icon is accepted (decoded, queued), not
once the panel has drawn it: SNI hosts fetch `IconPixmap` when they
choose.

## Events

```rust
#[istmo::message]
#[non_exhaustive]
pub enum TrayEvent {
    Activate(Pointer),               // primary click with PrimaryAction::Activate
    SecondaryActivate(Pointer),      // middle click
    DoubleClick(Pointer),            // Windows only
    Scroll(ScrollDelta),
    MenuOpened,
    MenuClosed,
    ItemActivated(ItemId),           // Action, or a check / radio (after auto toggle)
    CheckChanged(CheckChange),
    RadioSelected(RadioChange),
    Host(HostState),                 // tray host appeared, vanished, restarted
}

#[istmo::message]
pub struct Pointer { pub anchor: Option<Anchor> }

#[istmo::message]
pub struct Anchor {
    pub rect: Rect,                  // screen coordinates, physical pixels
    pub scale: f64,                  // of the monitor holding the rect
    pub precision: AnchorPrecision,
}

#[istmo::message]
#[non_exhaustive]
pub enum AnchorPrecision {
    IconRect,                        // the icon's own rect (Windows, macOS)
    ClickPoint,                      // a point, rect is 0×0 (X11 SNI hosts)
    Unreliable,                      // host-provided point of unknown meaning (Wayland SNI)
}
```

The anchor is in v1 even without popovers: apps that open their own
window near the icon can already do so on Windows, macOS and X11.

Delivery: one event hub per tray. Events emitted while nobody listens
are kept (last 64) and replayed to the first subscriber, same as
screen-capture's `EventHub`. `ItemActivated` etc. are never coalesced.

`Host(HostState)`: `Available`, `Missing` (Linux: no
`StatusNotifierWatcher`, or a watcher with no host registered), and
`Restarted` (explorer.exe restarted, panel restarted; the plugin has
already re-added the item). `create` succeeds without a host: the item
appears when a host does. Apps that need a fallback read
`capabilities().host` or the event.

## Capability report

```rust
#[istmo::message]
pub struct TrayCapabilities {
    pub backend: TrayBackend,        // Sni | Win32 | AppKit | Favicon | Unavailable
    pub host: HostState,
    pub max_items: Option<u32>,      // web: 1
    pub min_frame_ms: u32,
    pub image_names: Support,
    pub template_images: Support,
    pub title: Support,
    pub tooltip_body: Support,
    pub menu: MenuSupport,
    pub events: EventSupport,
    pub anchor: Option<AnchorPrecision>,
}

#[istmo::message]
#[non_exhaustive]
pub enum Support { Native, Emulated, HostDependent, Unsupported }

#[istmo::message]
pub struct MenuSupport {
    pub check: Support, pub mixed: Support, pub radio: Support,
    pub item_icons: Support, pub shortcuts: Support, pub header: Support,
    pub subtitle: Support, pub badge: Support, pub default_item: Support,
    pub mnemonics: Support, pub live_while_open: Support,
}

#[istmo::message]
pub struct EventSupport {
    pub activate: Support, pub secondary: Support, pub double_click: Support,
    pub scroll: Support, pub menu_open_close: Support,
}
```

Unsupported fields in a spec or patch are ignored and counted, never an
error (a menu written once must work everywhere). Exception: an image
that cannot be shown at all (`PlatformName` with no raster fallback) is
an error, because the result would be an invisible item.

### Expected values

| | Linux (SNI) | Windows | macOS | Web |
|---|---|---|---|---|
| check | Native | Native | Native | Unsupported |
| mixed | Native (`toggle-state` -1), host may draw it unchecked: HostDependent | Emulated (`hbmpChecked` dash glyph) | Native | Unsupported |
| radio | Native (`toggle-type=radio`), HostDependent rendering | Native (`MFT_RADIOCHECK`) | Emulated (`onStateImage` = SF Symbol `circle.fill`) | Unsupported |
| item icons | Native (`icon-data` PNG) | Native (`hbmpItem`, 32 bpp premultiplied) | Native | Unsupported |
| shortcuts (shown) | Native (`shortcut`) | Native (text after `\t`) | Native (`keyEquivalent`) | Unsupported |
| header | Emulated (disabled item) | Emulated (disabled item) | Native (macOS 14 `sectionHeader`), Emulated below | Unsupported |
| subtitle | Unsupported | Unsupported | Native (macOS 14) | Unsupported |
| badge | Unsupported | Unsupported | Native (macOS 14 `NSMenuItemBadge`) | Unsupported |
| default item | Unsupported | Native (`SetMenuDefaultItem`) | Emulated (bold attributed title) | Unsupported |
| mnemonics | Native (`_` in dbusmenu labels) | Native (`&`) | Unsupported (stripped) | Unsupported |
| live while open | HostDependent (KDE yes) | Unsupported (applies on next open) | Native | Unsupported |
| title text | HostDependent (`XAyatanaLabel`) | Unsupported | Native | Unsupported |
| tooltip body | Native (`ToolTip` description) | Emulated (joined, 127 chars) | Emulated (joined) | Unsupported |
| image names | Native (`IconName`) | Unsupported | Native (SF Symbols) | Unsupported |
| template | Unsupported | Unsupported | Native | Unsupported |
| activate | Native (`Activate`) | Native (`NIN_SELECT`) | Native | Unsupported |
| secondary | Native (`SecondaryActivate`) | Unsupported | Native (other mouse up) | Unsupported |
| double click | Unsupported | Native | Unsupported | Unsupported |
| scroll | Native (`Scroll`) | Unsupported | Emulated (local `NSEvent` monitor on the item's window) | Unsupported |
| menu open/close | Native (dbusmenu `opened`/`closed` events) | Native | Native (`NSMenuDelegate`) | Unsupported |
| anchor | ClickPoint (X11) / Unreliable (Wayland) | IconRect | IconRect | none |

Rows marked macOS 14 fall back to Emulated / Unsupported below 14,
decided at run time.

## Backends

### Linux: StatusNotifierItem + dbusmenu (zbus)

- zbus 5 with `async-io` (no tokio), on one plugin thread
  (`istmo-tray-dbus`) that owns the connection and every tray of the
  process.
- Exports per tray `org.kde.StatusNotifierItem` at
  `/StatusNotifierItem/<n>` and `com.canonical.dbusmenu` at
  `/MenuBar/<n>`:
  - SNI properties: `Category=ApplicationStatus`, `Id`, `Title`,
    `Status` (`Active` / `Passive` for hidden), `IconName`,
    `IconPixmap`, `IconThemePath`, `ToolTip`, `ItemIsMenu`
    (`PrimaryAction::ShowMenu`), `Menu`, `XAyatanaLabel`.
    Signals `NewIcon`, `NewTitle`, `NewToolTip`, `NewStatus`,
    `XAyatanaNewLabel`. Methods `Activate`, `SecondaryActivate`,
    `ContextMenu`, `Scroll`.
  - dbusmenu v3: `GetLayout`, `GetGroupProperties`, `GetProperty`,
    `Event`, `EventGroup`, `AboutToShow`, `AboutToShowGroup`; signals
    `LayoutUpdated(revision, parent)`, `ItemsPropertiesUpdated`.
    `ItemId(n)` maps to dbusmenu id `n + 1` (0 is the root).
- Registration: `RegisterStatusNotifierItem(object path)` on
  `org.kde.StatusNotifierWatcher`, the watcher taking the sender's
  unique name. No well-known name is owned, which keeps Flatpak to one
  `--talk-name=org.kde.StatusNotifierWatcher`. To verify per watcher
  (KDE, waybar, the GNOME AppIndicator extension, xfce4-panel): fall
  back to owning `org.kde.StatusNotifierItem-<pid>-<n>` where path
  registration is refused.
- `NameOwnerChanged` on the watcher name: re-register, emit
  `Host(Restarted)`. `IsStatusNotifierHostRegistered` +
  `StatusNotifierHostRegistered` drive `HostState`.
- `IconPixmap` = ARGB32 in network byte order, one entry per size
  (16, 22, 24, 32, 48, 64; SVG rasterised at each, rasters scaled).
  Item icons: PNG bytes (rasters passed through when already PNG of
  the right size, else re-encoded).
- Animation: one `NewIcon` per frame. `min_frame_ms` = 33 until
  measured on KDE / waybar.
- Labels: `&` mnemonics converted to dbusmenu `_`, literal `_` doubled.

GNOME without the AppIndicator extension has no watcher: `HostState::Missing`,
the item stays registered-pending and appears if the extension is
enabled later.

### Windows: `Shell_NotifyIconW`

- One thread (`istmo-tray-win`) with a hidden **top-level** window and
  a message loop. Not a message-only window: those do not receive the
  `TaskbarCreated` broadcast that tells us explorer.exe restarted (then
  every icon is re-added and `Host(Restarted)` emitted).
- Commands from other threads: queue + `PostMessageW` of a registered
  message.
- `NIM_SETVERSION` `NOTIFYICON_VERSION_4`: `NIN_SELECT` /
  `NIN_KEYSELECT` / `WM_CONTEXTMENU` / `WM_LBUTTONDBLCLK` /
  `WM_MBUTTONUP` with the anchor point in `wParam`;
  `Shell_NotifyIconGetRect` gives the icon rect.
- Identity: `uID` by default. `windows_guid` sets `NIF_GUID`, which lets
  Windows remember the user's "show in taskbar" choice across runs, but
  binds the registration to the executable path (moving the exe makes
  `NIM_ADD` fail). Opt-in, documented.
- Icons: `HICON` via `CreateIconIndirect` from premultiplied BGRA at
  `GetSystemMetricsForDpi(SM_CXSMICON)` for the taskbar's DPI;
  rebuilt on `WM_DPICHANGED` / `WM_SETTINGCHANGE`. Registered icons and
  animation frames are pre-built `HICON`s; a frame is one `NIM_MODIFY`.
- Menu: built fresh at open with `CreatePopupMenu` from the stored
  model, shown with `TrackPopupMenuEx(TPM_RETURNCMD)`, preceded by
  `SetForegroundWindow` and followed by `PostMessageW(WM_NULL)` (else
  the menu does not close on an outside click). Radio
  `MFT_RADIOCHECK`; mixed emulated with a dash `hbmpChecked`; default
  item `SetMenuDefaultItem`; shortcut text after `\t`.
- `WM_INITMENUPOPUP` / `WM_UNINITMENUPOPUP` → `MenuOpened` /
  `MenuClosed`.
- Windows 11 puts new icons in the overflow flyout and has no API to
  pin them. Documented, not worked around.
- DPI awareness is the app's (process-wide manifest); the plugin works
  under any mode.

### macOS: `NSStatusItem`

- objc2 0.6 + objc2-app-kit 0.3. Every AppKit call goes through
  `dispatch2::run_on_main` (as `istmo-dnd` does). The app must run an
  `NSApplication` event loop (winit / eframe do). For tray-only apps
  the crate exposes `istmo_tray::macos::run_event_loop(Options {
  dock_icon: bool })`, called from `main`; it sets the activation
  policy (`Accessory` without dock icon) and runs `NSApp`.
- `NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength)`,
  `autosaveName` = `TraySpec::id` (keeps the user's position across
  runs), `isVisible` for `set_visible`.
- Images: `NSImage` with one `NSBitmapImageRep` per backing scale
  (1×, 2×); SVG rasterised by the plugin (no reliance on NSImage SVG
  support). `PlatformName` → `NSImage(systemSymbolName:)` (macOS 11).
  `template` → `setTemplate(true)`.
- Title: `button.title` (attributed when the default item emulation
  needs bold).
- Menu: `NSMenu` built from the model; one Rust-defined target class
  (`define_class!`) whose action reads the item's `tag` (= `ItemId`).
  Patches mutate the existing `NSMenuItem`s (live while open: the main
  queue is drained in common run loop modes, event tracking included;
  to verify on hardware).
- `PrimaryAction::ShowMenu` → `statusItem.menu = menu`.
  `PrimaryAction::Activate` → no menu set, `button.sendActionOn(left |
  right | other mouse up)`; on right click the action sets the menu,
  calls `button.performClick`, and clears the menu again in
  `menuDidClose` (the supported way since `popUpStatusItemMenu` is
  deprecated).
- Anchor: `button.window.frame` converted to physical pixels with the
  screen's `backingScaleFactor`.

### Web: favicon

- `create` takes over the document's `<link rel="icon">` (creating one
  if none), keeping the original `href` to restore on `destroy`. One
  tray per document (`max_items = 1`); a second `create` fails with
  `TrayError::Limit`.
- Images become Blob URLs with their MIME type; no decoding, no
  `resvg` in the wasm build. `PlatformName` is an error. Animation via
  `setTimeout`.
- Browsers throttle timers in background tabs to about one per second,
  so an animation only plays at its speed while the tab is visible.
  Documented.
- Every other method answers `Unsupported`.

## Errors

```rust
#[istmo::message]
#[non_exhaustive]
pub enum TrayError {
    Unsupported(Feature),            // the call as a whole, e.g. set_menu on web
    UnknownTray,
    UnknownIcon,
    InvalidMenu(MenuError),          // from validate.rs, names the node
    Image(ImageError),               // which rep / frame and why
    Limit(LimitKind),
    Platform(String),                // OS call failed; message for logs
    ShutDown,
}
```

Hand-written `Display` / `Error`. Domain errors travel as
`IstmoError::PluginError` like every plugin.

## Build integration

The plugin has no Android / iOS native sources. Today an app with an
`android/` or `ios/` directory would generate a Kotlin / Swift
dispatcher and a registry entry for every handed-over contract, and the
registry would name a `TrayBackendImpl` that does not exist.

Needed change in `istmo-build` (step 0): `[plugin] targets = ["desktop",
"web"]` in the plugin's `istmo.toml`. `emit_app` skips Kotlin / Swift
codegen and registry entries for a contract whose plugin lists neither
`android` nor `ios`; auto-wiring (`ISTMO_AUTO_PLUGINS`) still includes
the client. Default (key absent) = every target, so existing plugins are
unchanged.

On Android / iOS the crate compiles to the `UnavailableTray` host; the
app registers it like the other `Unavailable*` hosts if it calls the
client there.

## Testing

- `istmo-menu`: validation and diff unit tests, property tests on the
  diff (patches applied to the old menu give the new one).
- Linux, automated: a private `dbus-daemon --session` from the dev
  shell, a test watcher + host written in the test (registers as
  `org.kde.StatusNotifierWatcher`, calls `GetLayout`, `Event`,
  `Activate`, `Scroll`), asserting layouts, property signals, events,
  re-registration after the watcher restarts, and coalescing.
  Workflow `tray.yml` via nix.
- Linux, manual checklist: KDE Plasma 6, waybar (niri, sway), GNOME +
  AppIndicator extension, xfce4-panel.
- Windows / macOS: cross clippy in CI (fenix stable combine), manual
  checklist on hardware; the anchor, explorer restart, DPI change and
  live-while-open rows are the ones to check.
- Web: wasm test in Node is not enough (needs a DOM): headless Chromium
  smoke like the webview browser host.
- `examples/tray-demo`: static icon, animated icon, registered icon
  cycling, every item kind, patches from a timer, events logged.

## Implementation order

0. `istmo-build`: `[plugin] targets`.
1. `istmo-menu`: model, validation, diff, shared type group.
2. `istmo-tray`: contract, generic host (handle table, coalescing,
   event hubs), image pipeline, `UnavailableTray`.
3. Linux backend + automated D-Bus tests.
4. Windows backend.
5. macOS backend.
6. Web favicon.
7. `tray-demo`, docs (en / es), CHANGELOG.

## Deferred

### Popover surfaces

The plugin creates a positioned surface of a size the app gives, and
the app renders into it through `raw-window-handle`: `NSPanel` on
macOS, a `WS_POPUP` window on Windows, an override-redirect window on
X11, a wlr-layer-shell surface on Wayland (anchored to the panel's
edge; GNOME has no layer-shell and gets an unpositioned toplevel,
reported as such). Because the surface lives on the plugin's own
connection / window procedure, the plugin also forwards its input
(pointer, keyboard through xkbcommon, touch, scale, configure, focus
loss): a small windowing layer for one surface. Handles are pointers,
so these methods admit `Local` origins only. `Anchor` in v1 is the
input it will need.

### Context menu and menu bar (macOS, Windows)

A second plugin on `istmo-menu`: a popup menu at a point of a window
registered in `istmo-window` (`NSMenu.popUp(positioning:at:in:)`,
`TrackPopupMenuEx` on the `HWND`), and the application menu bar
(`NSApp.mainMenu`, `SetMenu`). The menu bar needs standard roles
(About, Services, Hide, Quit, Edit's Copy / Paste so text fields work)
as a new `ItemKind` variant. Linux: no context menu (see Scope); a menu
bar only through KDE's global menu (`com.canonical.AppMenu.Registrar`
plus `org_kde_kwin_appmenu` on Wayland), reusing the dbusmenu server.

### Lazily filled submenus

`AboutToShow` (dbusmenu), `WM_INITMENUPOPUP` and `menuNeedsUpdate` all
expect the content synchronously on the platform thread, while the
app's answer would come back over an async call. A bounded wait on the
UI thread is the only way and it stalls the panel when the app is
slow. Left out until a consumer needs it; v1 apps keep the menu
current and `MenuOpened` tells them when it is about to matter.

## Rejected

- `ksni`: replaced by zbus directly (control over registration,
  re-registration and incremental updates, no tokio).
- libappindicator / GTK (as used by `tray-icon` / `muda` on Linux): a
  GTK dependency for a D-Bus protocol.
- A rendering API (text into icons, `tiny-skia`): apps supply images.
- XEmbed tray.
- A recursive menu type: does not derive, and the flat list diffs
  more simply.
- Message-only window on Windows: misses `TaskbarCreated`.
