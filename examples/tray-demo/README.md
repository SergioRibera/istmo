# tray-demo

A tray-only desktop app built on `istmo-tray`:

- a menu with a section header, a disabled clock item updated every
  second, a check item, a radio group in a submenu, an item with a
  subtitle and a badge, separators and a shortcut;
- three icon modes picked from the menu: a static SVG, an animated
  spinner (one call, the plugin's timer runs it) and battery icons
  registered once and swapped by handle every second;
- the clock as the item's title when the check item is on (macOS, and
  Linux panels that show `XAyatanaLabel`).

Every event is printed.

```sh
cargo run -p tray-demo
```

Linux needs a StatusNotifierItem host: KDE Plasma, waybar, xfce4-panel,
or GNOME with the AppIndicator extension. On macOS the demo runs the
main run loop itself (`istmo_tray::run_event_loop`) and stays out of the
Dock.
