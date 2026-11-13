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
- **Windows Service** — PowerShell install / uninstall scripts around
  `sc.exe`.
- **XDG `.desktop`** — Linux application entries.

They are plain functions: call them from a `build.rs`, a packaging
`xtask`, or a test, and write the returned string wherever your
packaging expects it. `emit()` does not call them.

## The service contract

systemd, launchd and Windows share one description,
`DesktopServiceContract`. `DesktopServiceContract::new(name,
description, exec_path)` fills the defaults; set the rest as fields:

| Field | Default | Used for |
| --- | --- | --- |
| `name` | — | systemd header comment |
| `label` | `name` | launchd `Label`, Windows service name |
| `description` | — | systemd `Description`, Windows description / display name |
| `exec_path`, `args` | — / `[]` | the command line |
| `working_directory` | `None` | systemd, launchd |
| `env` | `[]` | systemd `Environment`, launchd `EnvironmentVariables`, Windows registry |
| `restart` | `RestartPolicy::OnFailure` | `Never` / `OnFailure` / `Always` |
| `start_type` | `StartType::Manual` | `Auto` installs / loads at boot or login |
| `scope` | `ServiceScope::User` | `User` or `System` |
| `user`, `group` | `None` | system services only |
| `after` | `[]` | systemd `After=` |
| `windows_display_name`, `windows_dependencies` | `None` / `[]` | `sc.exe` `DisplayName=` / `depend=` |
| `launchd_keep_alive` | `None` (from `restart`) | launchd `KeepAlive` |
| `stop_timeout_seconds` | `30` | systemd `TimeoutStopSec`, launchd `ExitTimeOut` |

## systemd unit

```rust
use istmo_build::{
    DesktopServiceContract, RestartPolicy, ServiceScope, StartType, generate_systemd_unit,
};

fn main() {
    let mut contract = DesktopServiceContract::new(
        "acme-sync",
        "Acme background sync",
        "/usr/local/bin/acme-sync",
    );
    contract.scope = ServiceScope::System;
    contract.start_type = StartType::Auto;
    contract.restart = RestartPolicy::OnFailure;
    contract.env = vec![("RUST_LOG".into(), "info".into())];

    let unit = generate_systemd_unit(&contract);
    std::fs::write("packaging/acme-sync.service", unit).unwrap();
}
```

With `StartType::Auto` the unit gets an `[Install]` section
(`WantedBy=multi-user.target` for system units, `default.target` for
user units). Ship the file inside your distribution package — `.deb`,
`.rpm`, or `install.sh` — and it wires into `systemctl` normally.

## launchd plist

```rust
use istmo_build::{DesktopServiceContract, generate_launchd_plist};

let mut contract = DesktopServiceContract::new(
    "acme-sync",
    "Acme background sync",
    "/usr/local/bin/acme-sync",
);
contract.label = "com.acme.sync".into();
let plist = generate_launchd_plist(&contract);
std::fs::write("packaging/com.acme.sync.plist", plist).unwrap();
```

`StartType::Auto` sets `RunAtLoad`. User agent →
`~/Library/LaunchAgents/`; system daemon → `/Library/LaunchDaemons/`.
`launchctl load` picks it up.

## Windows service

Returns install + uninstall PowerShell scripts:

```rust
use istmo_build::generate_windows_service;

let artifacts = generate_windows_service(&contract);
std::fs::write("packaging/install.ps1", artifacts.install_script).unwrap();
std::fs::write("packaging/uninstall.ps1", artifacts.uninstall_script).unwrap();
```

The install script runs `sc.exe create` (plus `sc.exe description`,
`sc.exe failure` restart actions, and registry entries for `env`, which
`sc.exe` cannot set); the uninstall script stops and deletes the
service. Both need an elevated PowerShell. The binary itself must speak
the Service Control Manager protocol (`SetServiceStatus`); wire that
with a crate such as `windows-service`.

## XDG desktop entry

For foreground desktop apps:

```rust
use istmo_build::{DesktopAppContract, generate_desktop_entry};

let mut app = DesktopAppContract::new("Acme", "/usr/local/bin/acme %F");
app.icon = Some("acme".into());
app.categories = vec!["Utility".into(), "Office".into()];
app.mime_types = vec!["application/pdf".into()];

let entry = generate_desktop_entry(&app);
std::fs::write("packaging/acme.desktop", entry).unwrap();
```

The other fields are `comment`, `keywords`, `visible` (default `true`;
`false` writes `NoDisplay=true`), `terminal` (default `false`) and
`startup_notify` (default `true`). Drop the file into
`~/.local/share/applications/` or your distribution package's
`usr/share/applications/`.

## What the build already does for desktop

The generators above are opt-in, but `istmo_build::emit()` covers a few
desktop concerns on every build of an app crate with a binary target:

- the window icon behind `istmo::app_icon!()` and, on Windows, the
  `.exe` icon and application manifest (see
  [Native projects](/istmo/build-scripts/native-projects/#desktop));
- `[[assets]]` staged for `istmo::assets` (see the
  [`[[assets]]` reference](/istmo/build-scripts/istmo-toml-reference/#assets));
- `ISTMO_SINGLE_INSTANCE` for document-opening apps (see
  [`[[app.document_type]]`](/istmo/build-scripts/istmo-toml-reference/#appdocument_type)).

## iOS background

Even though this section is about desktop, iOS's background execution
plumbing lives in a sibling module worth mentioning here:

- `IosBackgroundContract` + `generate_ios_background` produce a Swift
  shim and the `Info.plist` fragment. For `refresh` / `processing`
  kinds the shim is an enum with `register()` (call it from
  `application(_:didFinishLaunchingWithOptions:)`) and `schedule()`; it
  runs the plugin's `on_start` through the runtime and, when iOS expires
  the task, cancels it with `IstmoRuntime.shared.cancel(pluginId:)`.
- `required_entitlements` returns the entitlements the background mode
  needs (the VoIP PushKit entitlement for `continuous_mode = "voip"`).

Plugins normally declare this in `[plugin.ios_background]` instead, and
the app build generates the shim — see
[Services and workers](/istmo/writing-plugins/services-workers/).

## Next

- Full `[app]` reference: [`istmo.toml` reference](/istmo/build-scripts/istmo-toml-reference/).
- Runtime auto-wiring: [Auto-wiring](/istmo/advanced/auto-wiring/).
