---
title: Deployment de desktop
description: Envía tu código Rust como service systemd, agent launchd, service Windows, o entry XDG desktop.
sidebar:
  order: 2
---

Istmo no es exclusivo de móvil — un ecosistema de plugins que escribes
una vez en Rust también corre en desktop, tanto como app foreground
como service background de larga vida.

El módulo `istmo_build::desktop` envía generadores para los cuatro
tipos canónicos de unit:

- **systemd** (`.service`) — units de sistema + usuario en Linux.
- **launchd** (`.plist`) — agents y daemons en macOS.
- **Windows Service** (`sc.exe`) — scripts de instalación para el SCM.
- **XDG `.desktop`** — entries de aplicación en Linux.

## Unit systemd

```rust
use istmo_build::{
    DesktopServiceContract, RestartPolicy, ServiceScope, StartType,
    generate_systemd_unit,
};

fn main() {
    let unit = generate_systemd_unit(&DesktopServiceContract {
        name: "acme-sync".into(),
        description: "Sync background Acme".into(),
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

Envía el unit generado dentro de tu paquete de distribución — `.deb`,
`.rpm`, o `install.sh` — y se cablea a `systemctl` normalmente.

## Plist launchd

```rust
use istmo_build::{DesktopServiceContract, generate_launchd_plist};

let plist = generate_launchd_plist(&DesktopServiceContract {
    name: "com.acme.sync".into(),
    // ...
});
std::fs::write("packaging/com.acme.sync.plist", plist).unwrap();
```

Agent de usuario → `~/Library/LaunchAgents/`; daemon de sistema →
`/Library/LaunchDaemons/`. `launchctl load` lo recoge.

## Service Windows

Retorna scripts de install + uninstall:

```rust
use istmo_build::generate_windows_service;

let artifacts = generate_windows_service(&contract);
std::fs::write("packaging/install.ps1", artifacts.install).unwrap();
std::fs::write("packaging/uninstall.ps1", artifacts.uninstall).unwrap();
```

Los scripts llaman `sc.exe create` / `delete` — el binario runtime
debe implementar el handshake SCM (`SetServiceStatus`). Un futuro
crate `istmo-desktop` va a envolver esto para que tu binario Rust
pueda registrar handlers con un decorator; hoy, cablealo manualmente
con el crate `winapi`.

## Entry XDG desktop

Para apps foreground desktop:

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

Coloca en `~/.local/share/applications/` o en el
`usr/share/applications/` de tu paquete de distribución.

## Dónde encaja esto

Los generadores desktop de `istmo-build` son **output puro** — toman
un contract, retornan un string. Nada corre el binario target ni lo
registra. Eso es de tu pipeline de packaging.

Para un "binario Rust → systemd + launchd + Windows Service" one-command
install, ver el (roadmap) crate `istmo-desktop`.

## Background iOS

Aunque esta sección es sobre desktop, la plomería de ejecución
background iOS vive en un módulo hermano que vale la pena mencionar
aquí:

- `IosBackgroundContract` + `generate_ios_background` producen el
  snippet `Info.plist` + código handler `AppDelegate` para
  `BGTaskScheduler` (`BGAppRefreshTask`, `BGProcessingTask`).
- `required_entitlements` retorna el fragmento `.entitlements` que tu
  proyecto Xcode necesita.

Cablea al lado de un plugin marcado `#[istmo::service]`; el runtime
inyecta el token de cancelación en tu `on_start` cuando iOS expira la
task.

## Siguiente

- Referencia completa `[app]`: [Referencia de `istmo.toml`](/istmo/es/build-scripts/istmo-toml-reference/).
- Auto-wiring del runtime: [Auto-wiring](/istmo/es/advanced/auto-wiring/).
