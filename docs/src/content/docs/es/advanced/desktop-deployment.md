---
title: Despliegue en escritorio
description: Distribuye tu código Rust como servicio systemd, agente launchd, servicio de Windows o entrada de escritorio XDG.
sidebar:
  order: 2
---

Istmo no es exclusivo de móvil — un ecosistema de plugins que escribes
una vez en Rust también corre en escritorio, tanto como app en primer
plano como servicio de larga duración en segundo plano.

El módulo `istmo_build::desktop` trae generadores para los cuatro tipos
de unidad canónicos:

- **systemd** (`.service`) — unidades de sistema y de usuario en Linux.
- **launchd** (`.plist`) — agentes y daemons de macOS.
- **Servicio de Windows** — scripts PowerShell de instalación /
  desinstalación alrededor de `sc.exe`.
- **XDG `.desktop`** — entradas de aplicación en Linux.

Son funciones simples: llámalas desde un `build.rs`, un `xtask` de
empaquetado o un test, y escribe el string devuelto donde lo espere tu
empaquetado. `emit()` no las llama.

## El contrato del servicio

systemd, launchd y Windows comparten una descripción,
`DesktopServiceContract`. `DesktopServiceContract::new(name,
description, exec_path)` llena los defaults; el resto se asigna como
campos:

| Campo | Default | Se usa para |
| --- | --- | --- |
| `name` | — | comentario de cabecera de systemd |
| `label` | `name` | `Label` de launchd, nombre del servicio de Windows |
| `description` | — | `Description` de systemd, descripción / nombre visible en Windows |
| `exec_path`, `args` | — / `[]` | la línea de comandos |
| `working_directory` | `None` | systemd, launchd |
| `env` | `[]` | `Environment` de systemd, `EnvironmentVariables` de launchd, registro de Windows |
| `restart` | `RestartPolicy::OnFailure` | `Never` / `OnFailure` / `Always` |
| `start_type` | `StartType::Manual` | `Auto` instala / carga al arrancar o al iniciar sesión |
| `scope` | `ServiceScope::User` | `User` o `System` |
| `user`, `group` | `None` | sólo servicios de sistema |
| `after` | `[]` | `After=` de systemd |
| `windows_display_name`, `windows_dependencies` | `None` / `[]` | `DisplayName=` / `depend=` de `sc.exe` |
| `launchd_keep_alive` | `None` (según `restart`) | `KeepAlive` de launchd |
| `stop_timeout_seconds` | `30` | `TimeoutStopSec` de systemd, `ExitTimeOut` de launchd |

## Unidad systemd

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

Con `StartType::Auto` la unidad lleva una sección `[Install]`
(`WantedBy=multi-user.target` en unidades de sistema, `default.target`
en las de usuario). Incluye el archivo en tu paquete de distribución —
`.deb`, `.rpm` o `install.sh` — y se integra con `systemctl` con
normalidad.

## Plist de launchd

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

`StartType::Auto` activa `RunAtLoad`. Agente de usuario →
`~/Library/LaunchAgents/`; daemon de sistema → `/Library/LaunchDaemons/`.
`launchctl load` lo recoge.

## Servicio de Windows

Devuelve scripts PowerShell de instalación y desinstalación:

```rust
use istmo_build::generate_windows_service;

let artifacts = generate_windows_service(&contract);
std::fs::write("packaging/install.ps1", artifacts.install_script).unwrap();
std::fs::write("packaging/uninstall.ps1", artifacts.uninstall_script).unwrap();
```

El script de instalación ejecuta `sc.exe create` (más `sc.exe
description`, las acciones de reinicio de `sc.exe failure` y entradas de
registro para `env`, que `sc.exe` no puede definir); el de
desinstalación detiene y borra el servicio. Ambos necesitan un
PowerShell elevado. El binario mismo debe hablar el protocolo del
Service Control Manager (`SetServiceStatus`); conéctalo con un crate como
`windows-service`.

## Entrada de escritorio XDG

Para apps de escritorio en primer plano:

```rust
use istmo_build::{DesktopAppContract, generate_desktop_entry};

let mut app = DesktopAppContract::new("Acme", "/usr/local/bin/acme %F");
app.icon = Some("acme".into());
app.categories = vec!["Utility".into(), "Office".into()];
app.mime_types = vec!["application/pdf".into()];

let entry = generate_desktop_entry(&app);
std::fs::write("packaging/acme.desktop", entry).unwrap();
```

Los demás campos son `comment`, `keywords`, `visible` (default `true`;
`false` escribe `NoDisplay=true`), `terminal` (default `false`) y
`startup_notify` (default `true`). Copia el archivo a
`~/.local/share/applications/` o al `usr/share/applications/` de tu
paquete de distribución.

## Lo que el build ya hace para escritorio

Los generadores de arriba son opcionales, pero `istmo_build::emit()`
cubre algunos temas de escritorio en cada build de un crate de app con
target binario:

- el ícono de ventana detrás de `istmo::app_icon!()` y, en Windows, el
  ícono del `.exe` y el manifest de aplicación (ver
  [Proyectos nativos](/istmo/es/build-scripts/native-projects/#escritorio));
- los `[[assets]]` preparados para `istmo::assets` (ver la
  [referencia de `[[assets]]`](/istmo/es/build-scripts/istmo-toml-reference/#assets));
- `ISTMO_SINGLE_INSTANCE` para apps que abren documentos (ver
  [`[[app.document_type]]`](/istmo/es/build-scripts/istmo-toml-reference/#appdocument_type)).

## Background en iOS

Aunque esta sección trata de escritorio, la plomería de ejecución en
segundo plano de iOS vive en un módulo hermano que vale la pena
mencionar:

- `IosBackgroundContract` + `generate_ios_background` producen un shim
  Swift y el fragmento de `Info.plist`. Para los tipos `refresh` /
  `processing` el shim es un enum con `register()` (llámalo desde
  `application(_:didFinishLaunchingWithOptions:)`) y `schedule()`;
  ejecuta el `on_start` del plugin a través del runtime y, cuando iOS
  expira la tarea, la cancela con `IstmoRuntime.shared.cancel(pluginId:)`.
- `required_entitlements` devuelve los entitlements que necesita el modo
  de background (el entitlement de VoIP PushKit para
  `continuous_mode = "voip"`).

Normalmente los plugins declaran esto en `[plugin.ios_background]`, y el
build de la app genera el shim — ver
[Services y workers](/istmo/es/writing-plugins/services-workers/).

## Siguiente

- Referencia completa de `[app]`: [Referencia de `istmo.toml`](/istmo/es/build-scripts/istmo-toml-reference/).
- Auto-wiring del runtime: [Auto-wiring](/istmo/es/advanced/auto-wiring/).
