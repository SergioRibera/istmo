---
title: El manifest
description: Qué es istmo.toml, por qué existe y cómo fluye cada sección por el build.
sidebar:
  order: 4
---

`istmo.toml` vive junto a `Cargo.toml` y declara todo lo que el
framework necesita saber y que no se puede derivar del código Rust:

- El identificador del plugin y el path del tipo client en Rust.
- El deployment por defecto (local vs. proceso `:remote`).
- Dependencias nativas — coordenadas Gradle y paquetes SwiftPM.
- Necesidades de plataforma — permisos de Android, entradas de
  `Info.plist`, versiones mínimas del SO, assets empaquetados.
- Identidad de la app y ajustes de codegen (sección `[app]`).

Tanto los crates de plugin como los de app pueden tener un `istmo.toml`.
El archivo es opcional — si falta, la configuración está vacía — pero un
crate de plugin lo necesita para que su contrato llegue a la app:
`emit()` sólo extrae los contratos de las entradas `[plugin]` que tienen
`client_type`.

## Los tres sabores

### Crate de plugin con un solo plugin

La forma más común:

```toml
[plugin]
id              = "acme.telemetry"
client_type     = "::acme_telemetry::TelemetryClient"
android_package = "com.acme.telemetry"

[[gradle]]
group    = "androidx.work"
artifact = "work-runtime-ktx"
version  = "2.9.0"

[[swift_package]]
url          = "https://github.com/apple/swift-log"
product      = "Logging"
from_version = "1.5.0"
```

`istmo_build::emit()` lee esto, extrae el contrato del trait `Telemetry`
de `src/lib.rs` y lo entrega como `DEP_<links>_ISTMO_CONTRACTS`, además
del manifest parseado como `DEP_<links>_ISTMO_MANIFEST` y las
dependencias nativas como `DEP_<links>_ISTMO_NATIVE_DEPS`.

### Crate de plugin con varios plugins

Un crate puede publicar varios traits de plugin con la forma de array
`[[plugin]]`:

```toml
[[plugin]]
id          = "acme.permissions"
client_type = "::acme_platform::PermissionsClient"

[[plugin]]
id          = "acme.notifications"
client_type = "::acme_platform::NotificationsClient"

# Dependencias nativas compartidas por todos los plugins del crate.
[[gradle]]
group    = "androidx.core"
artifact = "core-ktx"
version  = "1.13.0"
```

`istmo_build::emit()` recorre cada entrada `[[plugin]]`, extrae un
contrato por trait y los entrega todos en un solo payload, así que la
app no necesita `extra_contracts` para ellos. Dos entradas con el mismo
`id` hacen fallar el build.

### Manifest del lado de la app

Un crate de app usa `istmo.toml` para su identidad, sus ajustes de
codegen y sus overrides:

```toml
[app]
id              = "com.myapp"
name            = "My App"
android_package = "com.myapp.gen"
ios_app_dir     = "MyApp"

[[app.plugin]]
id        = "istmo.echo"
role      = "client"
platforms = ["android"]

[[remote_override]]
plugin     = "acme.large_model"
deployment = "remote"

[min_versions]
android = 24
ios     = "15.0"
```

Todo lo que va bajo `[app]` y `[[app.plugin]]` da forma a la identidad
de la app y al codegen Kotlin/Swift — ver
[visión general de `emit()`](/istmo/es/build-scripts/emit-overview/) y la
[referencia completa](/istmo/es/build-scripts/istmo-toml-reference/).

`[[remote_override]]` cambia el deployment por defecto de un plugin, de
modo que el auto-wiring lo declara remoto y sus frames pasan por el
puente `:remote` en lugar del transporte local.

## Qué vive fuera del manifest

- **Las firmas del trait.** Vienen de tu código Rust; `syn` las parsea
  a un `Contract`.
- **El versionado del wire.** El protocolo lleva su propia versión
  (`PROTOCOL_VERSION` en `istmo-core`).
- **El wiring del runtime.** La invocación de la macro
  `istmo::runtime!`, no TOML.
- **Entradas de manifests de plataforma.** Providers, receivers, intent
  filters, claves extra de `Info.plist` y entitlements se envían como
  archivos junto a las fuentes nativas
  (`native/android/AndroidManifest.xml`, `native/ios/Info.plist.fragment`,
  …) y se fusionan en la app — ver
  [fragmentos de manifiestos nativos](/istmo/es/build-scripts/istmo-toml-reference/#fragmentos-de-manifiestos-nativos).
  Los permisos, las descripciones de uso y las versiones mínimas del SO
  sí viven en el manifest (`android_permissions`, `[plugin.info_plist]`,
  [`[min_versions]`](/istmo/es/build-scripts/istmo-toml-reference/#min_versions)).

Mantener la frontera explícita significa que nunca persigues un cambio
por dos archivos.

## Siguiente

- Referencia campo por campo: [Referencia de `istmo.toml`](/istmo/es/build-scripts/istmo-toml-reference/).
- Entiende cómo la sección `[app]` guía el codegen: [Visión general de
  build scripts](/istmo/es/build-scripts/emit-overview/).
