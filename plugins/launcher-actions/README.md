# istmo-launcher-actions

The app's icon where the system launches it, for the
[istmo](https://github.com/SergioRibera/istmo) framework: launcher
shortcuts and Home Screen quick actions, Jump List tasks, the Dock
menu, badges, progress bars, attention requests and recent documents.

An action is an id, a title and a URL. Activating it publishes a
`DeepLink` on the `istmo.deeplinks` queue, so the app routes launcher
actions with the router it already uses for links.

| | Android | iOS | Windows | macOS | Linux |
| --- | --- | --- | --- | --- | --- |
| Dynamic actions | `ShortcutManager` (API 25+) | `shortcutItems` (4 shown) | Jump List tasks (10) | Dock menu (16) | static only |
| Pin | `requestPinShortcut` (API 26+) | | | | |
| Badge | | count | | label | count (LauncherEntry) |
| Progress | | | taskbar | | LauncherEntry |
| Attention | | | `FlashWindowEx` | `requestUserAttention` | LauncherEntry `urgent` |
| Recent documents | | | Jump List | Dock menu | |

Anything a platform does not do answers `LauncherError::Unsupported`;
`capabilities()` says what works.

## Install

```toml
[dependencies]
istmo-launcher-actions = "0.1.1-alpha.7"
```

## Usage

```rust,ignore
use istmo_launcher_actions::{Badge, LauncherAction, LauncherActionsClient};

let launcher = LauncherActionsClient::from_runtime(&runtime)?;
launcher
    .set_actions_checked(vec![LauncherAction {
        id: "compose".into(),
        title: "New note".into(),
        subtitle: None,
        url: "notes://compose".into(),
        icon: None,
    }])
    .await?;
launcher.set_badge(Badge::Count(3)).await?;
```

Static actions go in the app's `istmo.toml`:

```toml
[[app.launcher_action]]
id = "compose"
title = "New note"
url = "notes://compose"
ios_symbol = "square.and.pencil"
```

## Wiring

* Android: nothing to do; the shortcut trampoline
  `IstmoLauncherActionActivity` is merged from the plugin's manifest.
* iOS: forward quick actions from the scene delegate with
  `IstmoLauncherActions.perform(item)`.
* Desktop: register `DesktopLauncherActions` as the host, and
  `DocumentsPublisher` so argv URLs (Jump List tasks, `.desktop`
  actions) reach `DeepLinks`. Taskbar calls on Windows need the windows
  registered with `istmo-window`.

Full guide: <https://sergioribera.rs/istmo/plugins/launcher-actions/>
