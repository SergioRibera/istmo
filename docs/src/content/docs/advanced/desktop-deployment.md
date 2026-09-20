---
title: Desktop deployment
description: Ship your Rust code as a systemd service, launchd agent, Windows service, or XDG desktop entry.
sidebar:
  order: 2
---

Istmo is not exclusive to mobile — a plugin ecosystem you write once
in Rust also runs on desktop, both as a foreground app and as a
long-lived background service.

The `istmo_build::desktop` module ships generators for the four
canonical unit types:

- **systemd** (`.service`) — Linux system + user units.
- **launchd** (`.plist`) — macOS agents and daemons.
- **Windows Service** (`sc.exe`) — install scripts for the SCM.
- **XDG `.desktop`** — Linux application entries.

## systemd unit

```rust
use istmo_build::{
    DesktopServiceContract, RestartPolicy, ServiceScope, StartType,
    generate_systemd_unit,
};

fn main() {
    let unit = generate_systemd_unit(&DesktopServiceContract {
        name: "acme-sync".into(),
        description: "Acme background sync".into(),
        exec_start: "/usr/local/bin/acme-sync".into(),
        scope: ServiceScope::System,
        start_type: StartType::OnBoot,
        restart_policy: RestartPolicy::OnFailure,
        environment: vec![
            ("RUST_LOG".into(), "info".into()),
        ],
    });
    std::fs::write("packaging/acme-sync.service", unit).unwrap();
}
```

Ship the generated unit inside your distribution package — `.deb`,
`.rpm`, or `install.sh` — and it wires into `systemctl` normally.

## launchd plist

```rust
use istmo_build::{DesktopServiceContract, generate_launchd_plist};

let plist = generate_launchd_plist(&DesktopServiceContract {
    name: "com.acme.sync".into(),
    // ...
});
std::fs::write("packaging/com.acme.sync.plist", plist).unwrap();
```

User agent → `~/Library/LaunchAgents/`; system daemon →
`/Library/LaunchDaemons/`. `launchctl load` picks it up.

## Windows service

Returns install + uninstall scripts:

```rust
use istmo_build::generate_windows_service;

let artifacts = generate_windows_service(&contract);
std::fs::write("packaging/install.ps1", artifacts.install).unwrap();
std::fs::write("packaging/uninstall.ps1", artifacts.uninstall).unwrap();
```

The scripts call `sc.exe create` / `delete` — the runtime binary must
implement the SCM handshake (`SetServiceStatus`). A future
`istmo-desktop` crate will wrap this so your Rust binary can register
handlers with a decorator; today, wire it manually with the `winapi`
crate.

## XDG desktop entry

For foreground desktop apps:

```rust
use istmo_build::{DesktopAppContract, generate_desktop_entry};

let entry = generate_desktop_entry(&DesktopAppContract {
    name: "Acme".into(),
    exec: "/usr/local/bin/acme".into(),
    icon: "/usr/share/icons/hicolor/512x512/apps/acme.png".into(),
    categories: vec!["Utility".into(), "Productivity".into()],
});
std::fs::write("packaging/acme.desktop", entry).unwrap();
```

Drop into `~/.local/share/applications/` or your distribution package's
`usr/share/applications/`.

## Where these fit

`istmo-build`'s desktop generators are **pure output** — they take a
contract, they return a string. Nothing runs the target binary or
registers it. That's on your packaging pipeline.

For a full "Rust binary → systemd + launchd + Windows Service"
one-command install, see the (roadmap) `istmo-desktop` crate.

## iOS background

Even though this section is about desktop, iOS's background execution
plumbing lives in a sibling module worth mentioning here:

- `IosBackgroundContract` + `generate_ios_background` produce the
  `Info.plist` snippet + `AppDelegate` handler code for
  `BGTaskScheduler` (`BGAppRefreshTask`, `BGProcessingTask`).
- `required_entitlements` returns the `.entitlements` fragment your
  Xcode project needs.

Wire alongside a plugin marked `#[istmo::service]`; the runtime injects
the cancellation token into your `on_start` when iOS expires the task.

## Next

- Full `[app]` reference: [`istmo.toml` reference](/istmo/build-scripts/istmo-toml-reference/).
- Runtime auto-wiring: [Auto-wiring](/istmo/advanced/auto-wiring/).
