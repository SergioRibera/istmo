# istmo-tray

Tray icons for the [istmo](https://github.com/SergioRibera/istmo)
framework: a status item with a menu and a static or animated icon that
the app swaps at run time.

| Platform | Backend |
| --- | --- |
| Linux | `StatusNotifierItem` + `com.canonical.dbusmenu` over D-Bus (KDE Plasma, waybar, xfce4-panel, GNOME with the AppIndicator extension) |
| Windows | The notification area (`Shell_NotifyIconW`, `NOTIFYICON_VERSION_4`); menus built when they open; icons added again when explorer.exe restarts |
| macOS | The menu bar (`NSStatusItem` + `NSMenu`) on the main thread; radio items drawn with a dot, section headers, subtitles and badges on macOS 14 |
| Web | `WebTray`: the tab's favicon (Blob URLs, animations on `setTimeout`); menus, titles, tooltips and visibility are accepted and ignored |
| Android / iOS | `UnavailableTray`: every call answers `TrayError::Unsupported` |

Everything runs in Rust; the plugin generates no Kotlin or Swift.

## Install

```toml
[dependencies]
istmo-tray = "0.1.1-alpha.7"
```

Features: `svg` (default) rasterises SVG icons at the size each tray
host asks for; `jpeg` (default) decodes JPEG icons. PNG is always
supported.

## Usage

```rust,ignore
use istmo_tray::istmo_menu::{CheckState, IconImage, ImageData, MenuBuilder};
use istmo_tray::{TrayClient, TrayEvent, TraySpec};

let tray = TrayClient::from_runtime(&runtime)?;
let menu = MenuBuilder::default()
    .action(1, "&Open")
    .check(2, "Start at login", CheckState::Off)
    .separator(3)
    .action(4, "&Quit")
    .into_nodes();
let item = tray
    .create_owned(
        TraySpec::new("my-app", IconImage::new(ImageData::Svg(ICON.to_vec())))
            .with_tooltip("My app")
            .with_menu(menu),
    )
    .await?;

let events = tray.events(item.tray.id())?;
while let Ok(istmo::StreamItem::Event(event)) = events.recv_async().await {
    if event == TrayEvent::ItemActivated(4.into()) {
        break;
    }
}
```

Register the host for the platform: `TrayHost::new(DesktopTray::default())`
on desktop, `TrayHost::new(WebTray::default())` on the web,
`TrayHost::new(UnavailableTray)` on mobile. On macOS the app must
run the main run loop (winit and eframe do); an app whose only UI is
the tray calls `istmo_tray::run_event_loop(false)` from `main`, which
also keeps it out of the Dock.

## Icons

Icons are SVG, PNG or JPEG files (or platform icon names) supplied by
the app; the plugin never draws. An icon can list several
representations (an SVG, PNGs of several sizes, a theme name with a
raster fallback) and the plugin picks per size.

* `register_icon` decodes an icon once; swapping to it by handle is a
  lookup.
* `TrayIcon::Animated` is one call: the plugin's timer flips frames.
* Changes made faster than the platform applies them collapse to the
  latest.

## Menus

Menus are `istmo_menu` node lists: actions, checks, radio groups,
submenus, separators, headers, shortcuts, item icons. Checks toggle and
radio items select themselves before the app hears about the click
(`MenuBuilder::manual` turns that off). `set_menu` with the same items
in the same places is sent as property changes; `update_items` changes
single items.

`capabilities()` tells per feature whether the backend shows it
natively, emulates it, leaves it to the tray host or ignores it.
Unsupported properties are ignored, never refused, so one menu works
everywhere.
