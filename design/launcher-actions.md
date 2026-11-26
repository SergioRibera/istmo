# `istmo-launcher-actions`

Everything attached to the app's icon where the system launches it:
the Android launcher, the iOS Home Screen, the Windows taskbar, the
macOS Dock and the Linux dock / launcher.

- **Actions**: entries in the icon's menu (Android app shortcuts, iOS
  Home Screen quick actions, Windows Jump List tasks, macOS Dock menu,
  Linux `.desktop` `Actions=`). Static ones come from `istmo.toml`,
  dynamic ones from the plugin, pinned ones on request.
- **Badge**: a count or short label on the icon.
- **Progress**: a progress bar on the icon.
- **Attention**: bounce / flash / mark urgent.
- **Recent documents**: the "Recent" category of the Jump List and the
  Dock menu.

Global keyboard shortcuts are not part of this crate (a future
`istmo-hotkeys`).

## Activation = deep link

An action is a title, an optional icon and a URL. Activating it
publishes a [`DeepLink`] on the existing `istmo.deeplinks` queue, so an
app routes launcher actions with the same router it uses for links. No
new lane, nothing to subscribe to twice.

| Platform | How the URL comes back |
|---|---|
| Android | The shortcut's intent targets the plugin's `IstmoLauncherActionActivity` (not exported, `ACTION_VIEW` + data URI + id extra). It publishes the link, starts the launch intent of the package and finishes. Works whether or not the app handles `VIEW` intents. |
| iOS | `IstmoLauncherActions.perform(_ item: UIApplicationShortcutItem)` (one call from the scene delegate's `windowScene(_:performActionFor:)` and from `scene(_:willConnectTo:options:)` for cold starts). Same shape as `IstmoShareInbox`: iOS gives a library no way to observe quick actions. |
| Windows | A Jump List task is a shell link to the executable with the URL as its only argument. `DocumentsPublisher` already turns a scheme argument into a deep link, and forwards it to the running primary when single-instance is on. |
| Linux | `.desktop` `Exec=<app> <url>` per action (packaging writes it); same `DocumentsPublisher` path. |
| macOS | Dock menu items call back into the process and publish the link directly. |

`DeepLink.source` is `launcher-action:<id>` where the platform tells us
the id (Android, iOS, macOS). On Windows / Linux it is the argv source
(`launch` / `running`). Apps that care about the id route by URL.

The URL must have an RFC 3986 scheme. The app's own scheme
(`[[app.url_scheme]]`) is the expected choice; nothing on mobile or in
the Dock needs the scheme registered, but Windows / Linux need nothing
it either (argv, not the scheme handler). `http(s)` URLs are refused: an
action that opens the browser is the opener's job, and a link that looks
external is confusing inside the app's own router.

## Wire types

```rust
pub struct LauncherAction {
    pub id: String,                // 1..=64 [A-Za-z0-9._-], unique in a set
    pub title: String,             // short label (Android `shortLabel`)
    pub subtitle: Option<String>,  // Android `longLabel`, iOS subtitle, Windows tooltip
    pub url: String,
    pub icon: Option<ActionIcon>,
}

pub enum ActionIcon {
    /// Platform symbol name: SF Symbol on Apple, ignored elsewhere.
    Symbol(String),
    /// PNG bytes (Android bitmap icon, Windows `.ico` written to the
    /// plugin's cache dir, macOS `NSImage`).
    Png(Vec<u8>),
}

pub struct Capabilities {
    pub dynamic_actions: bool,
    pub max_actions: u32,          // 0 when actions are unsupported
    pub pin: bool,
    pub badge: BadgeSupport,       // Unsupported | Count | Label
    pub progress: bool,
    pub attention: bool,
    pub recent_documents: bool,
}

pub enum Badge { Clear, Count(u32), Label(String) }

pub enum Progress {
    Clear,
    Indeterminate,
    Normal(f32),                   // 0.0..=1.0
    Paused(f32),
    Error(f32),
}

pub enum Attention { Informational, Critical, Cancel }

pub enum PinOutcome { Requested, AlreadyPinned }

pub enum LauncherError {
    Unsupported,
    InvalidAction { id: String, reason: String },
    TooMany { max: u32 },
    RateLimited,                   // Android `ShortcutManager.isRateLimitingActive`
    NotPermitted,                  // iOS badge without notification permission
    NoWindow,                      // Windows taskbar calls with no registered window
    Backend(String),
}
```

Variants are never named after their payload type (Kotlin shadowing,
see the screen-capture notes). `LauncherError`, `Badge`, `Progress`,
`BadgeSupport` and `Attention` are `#[non_exhaustive]`.

## Trait

```rust
#[plugin(name = "istmo.launcher_actions", remote = false)]
pub trait LauncherActions {
    async fn capabilities(&self) -> Capabilities;
    /// Replace the dynamic actions (static ones stay).
    async fn set_actions(&self, actions: Vec<LauncherAction>) -> Result<(), LauncherError>;
    async fn clear_actions(&self) -> Result<(), LauncherError>;
    /// The user did the thing an action does from inside the app
    /// (Android ranks shortcuts by it). No-op elsewhere.
    async fn report_used(&self, id: String) -> Result<(), LauncherError>;
    async fn request_pin(&self, action: LauncherAction) -> Result<PinOutcome, LauncherError>;
    async fn set_badge(&self, badge: Badge) -> Result<(), LauncherError>;
    async fn set_progress(&self, progress: Progress) -> Result<(), LauncherError>;
    async fn request_attention(&self, attention: Attention) -> Result<(), LauncherError>;
    async fn add_recent_document(&self, path: String) -> Result<(), LauncherError>;
    async fn clear_recent_documents(&self) -> Result<(), LauncherError>;
}
```

`remote = false`: these mutate the app's own launcher presence; a
`:remote` process has no business doing it.

`LauncherAction::validate` / `LauncherActions::validate_set` run on the
Rust client before the call (id charset, unique ids, non-empty title,
URL with a scheme, not `http(s)`), so mobile backends only re-check
counts.

## Platform matrix (v1)

| | Android | iOS | Windows | macOS | Linux |
|---|---|---|---|---|---|
| dynamic actions | `ShortcutManager` (25+) | `shortcutItems` | Jump List tasks | Dock menu | no (static only) |
| max | `maxShortcutCountPerActivity` | 4 shown | 10 (we cap) | 16 (we cap) | 0 |
| pin | `requestPinShortcut` (26+) | no | no | no | no |
| badge | no (notifications own it) | Count (16+: `setBadgeCount`) | no (see below) | Label (`NSDockTile`) | Count (LauncherEntry) |
| progress | no | no | `ITaskbarList3` | no | LauncherEntry `progress` |
| attention | no | no | `FlashWindowEx` | `requestUserAttention` | LauncherEntry `urgent` |
| recent | no | no | `SHAddToRecentDocs` | `NSDocumentController` | no |

Windows badge: the overlay icon is an image, and the plugin never draws
(same rule as the tray). v1 reports `BadgeSupport::Unsupported` on
Windows; a later `Badge::Image(Vec<u8>)` can carry an app-drawn overlay.

Windows taskbar calls (progress, attention) apply to every window in the
`istmo-window` registry; with none registered they fail `NoWindow`.

Linux LauncherEntry: `com.canonical.Unity.LauncherEntry` `Update`
signal on the session bus, object `/dev/istmo/LauncherEntry`, app URI
`application://<desktop id>.desktop`. Desktop id = `FLATPAK_ID`, else
`DesktopLauncherActions::with_desktop_id`, else the build's
`ISTMO_APP_BUNDLE_ID`. The object also answers `Query` (docks call it
when they start after the app). Supported by KDE Plasma, Dash to Dock /
Dash to Panel, Plank; a dock that does not listen just ignores it, so
`capabilities` cannot tell (reported as supported).

macOS Dock menu: AppKit only asks the app delegate
(`applicationDockMenu:`). The backend adds that method to the delegate's
class with `class_addMethod` when the class does not already implement
it; when it does (the app has its own Dock menu), `set_actions` fails
`Unsupported` and logs why. Runs on the main thread (`dispatch2`).

## Static actions: `[[app.launcher_action]]`

```toml
[[app.launcher_action]]
id = "compose"
title = "New note"
subtitle = "Start an empty note"   # optional
url = "notes://compose"
icon = "assets/compose.png"         # optional; .png or Android .xml vector
ios_symbol = "square.and.pencil"   # optional SF Symbol
```

Parsed by `istmo-build` (`launcher_actions.rs`, same shape as
`url_schemes.rs`, same validation as the Rust client).

- Android: `res/xml/istmo_launcher_actions.xml` (`<shortcuts>`, intent
  `targetPackage` = `[app] id`, `targetClass` = the trampoline) plus
  string resources, written to the generated `.istmo/res`, and a
  `<meta-data android:name="android.app.shortcuts">` patched onto the
  launcher activity next to the URL-scheme filters. A build with
  `applicationIdSuffix` breaks `targetPackage`; documented.
- iOS: `UIApplicationShortcutItems` in the Info.plist patch, `type` =
  id, `userInfo.url` = URL, `UIApplicationShortcutItemIconSymbolName`.
- Desktop: packaging reads the table from `istmo.toml` (`.desktop`
  `Actions=`), like `[[app.url_scheme]]`. Windows / macOS static actions
  are dynamic ones the app sets at start (neither platform has a static
  form an unpackaged app can ship).

## Crate layout

```
plugins/launcher-actions/
  istmo.toml                 id, android_package dev.istmo.plugins.launcher_actions, backends
  src/lib.rs                 types, trait, validation, client helpers
  src/desktop/mod.rs         DesktopLauncherActions (dispatch per OS)
  src/desktop/linux.rs       LauncherEntry over zbus (no unsafe)
  src/desktop/windows.rs     COM: ICustomDestinationList, ITaskbarList3, FlashWindowEx, SHAddToRecentDocs (allow unsafe_code)
  src/desktop/macos.rs       NSDockTile, Dock menu, requestUserAttention, NSDocumentController (allow unsafe_code)
  native/android/            LauncherActionsBackendImpl.kt, IstmoLauncherActionActivity.kt, AndroidManifest.xml
  native/ios/                LauncherActionsBackendImpl.swift, IstmoLauncherActions.swift
  tests/                     validation, codegen goldens, Linux LauncherEntry against a private bus
```

`unsafe_code = "deny"` with per-module `allow` on the Windows and macOS
modules, as in `istmo-dnd` / `istmo-webview`.

## Not in v1

- Alternate app icons (iOS `setAlternateIconName`, Android activity
  aliases): build-time assets per icon.
- Windows thumbnail toolbar buttons (per window, closer to
  `istmo-window`).
- macOS Dock progress (custom `NSDockTile` content view: drawing).
- Linux recent documents (`recently-used.xbel`).
- Badge images on Windows.
