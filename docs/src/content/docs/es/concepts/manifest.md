---
title: El manifest
description: Qué es istmo.toml, por qué existe, y cómo fluye cada sección a través del build.
sidebar:
  order: 4
---

`istmo.toml` se sienta al lado de `Cargo.toml` y declara todo lo que el
framework necesita saber que no es derivable desde código Rust:

- El identificador del plugin (un string, no el nombre del crate).
- El path del tipo cliente Rust.
- Defaults de deployment (local vs. proceso `:remote`).
- Dependencias nativas — coordenadas Gradle y paquetes SwiftPM.
- Overrides app-side del codegen (sección `[app]`).

Tanto crates plugin como crates app pueden cargar un `istmo.toml`. El
archivo es opcional para crates plugin 100% Rust que no envían nada
nativo, pero la convención es: **si tu crate toca Istmo, tiene un
`istmo.toml`.**

## Los tres flavors

### Crate plugin single-plugin

La forma más común:

```toml
[plugin]
id          = "acme.telemetry"
client_type = "::acme_telemetry::TelemetryClient"

[[gradle]]
group    = "androidx.work"
artifact = "work-runtime-ktx"
version  = "2.9.0"

[[swift_package]]
url          = "https://github.com/apple/swift-log"
product      = "Logging"
from_version = "1.5.0"
```

`istmo_build::emit()` lee esto, verifica que el trait existe, y emite
un contrato bincoded a `DEP_<crate>_ISTMO_CONTRACT`, más las deps
nativas a `DEP_<crate>_NATIVE_DEPS`.

### Crate plugin multi-plugin

Raro pero soportado — un crate publicando varios traits de plugin
(ej. `istmo-plugins` empaqueta Permissions, Notifications, AdMob):

```toml
[[plugin]]
id          = "istmo.permissions"
client_type = "::istmo_plugins::PermissionsClient"

[[plugin]]
id          = "istmo.notifications"
client_type = "::istmo_plugins::NotificationsClient"

# Deps nativas compartidas entre cada plugin en el crate.
[[gradle]]
group    = "androidx.core"
artifact = "core-ktx"
version  = "1.13.0"
```

`istmo_build::emit()` itera cada entrada `[[plugin]]` y extrae un
contrato por trait.

### Manifest app-side

Un crate app usa `istmo.toml` sólo para **overrides**:

```toml
[app]
android         = true
android_package = "com.myapp.gen"
ios             = true
ios_app_dir     = "MyApp"

[[app.plugin]]
id        = "istmo.echo"
role      = "client"
platforms = ["android"]

[[remote_override]]
plugin     = "istmo.push_notifications"
deployment = "remote"
```

Todo bajo `[app]` y `[[app.plugin]]` da forma al codegen Kotlin/Swift
— ver [vista general de `emit_app`](/istmo/es/build-scripts/emit-overview/)
y la [referencia completa](/istmo/es/build-scripts/istmo-toml-reference/).

`[[remote_override]]` flippea el deployment default de un plugin para
que la clase cliente rutee a través del bridge `:remote` en vez del
runtime local.

## Qué vive afuera del manifest

- **Los signatures de traits.** Vienen de tu fuente Rust; `syn` los
  parsea a un `Contract`.
- **Versionado del wire.** El protocolo carga su propio byte de
  versión (`PROTOCOL_VERSION` en `istmo-core`).
- **Wiring del runtime.** Invocación de la macro `istmo::runtime!`,
  no TOML.

Mantener la frontera explícita significa que nunca persigues un cambio
en dos archivos.

## Siguiente

- Referencia campo por campo: [Referencia de `istmo.toml`](/istmo/es/build-scripts/istmo-toml-reference/).
- Entiende cómo la sección `[app]` app-side guía el codegen: [Vista
  general de build scripts](/istmo/es/build-scripts/emit-overview/).
