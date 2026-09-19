---
title: Referencia istmo.toml
description: Cada clave, cada default, cada valor válido.
sidebar:
  order: 2
---

Cada crate plugin y cada crate app puede tener un `istmo.toml`. Cada
sección es opcional; un archivo vacío es legal.

## Tables top-level

| Clave                | Tipo              | Dónde       | Propósito                                     |
| -------------------- | ----------------- | ----------- | --------------------------------------------- |
| `[plugin]`           | tabla / array     | plugin      | Declara uno o más traits de plugin            |
| `[[gradle]]`         | array de tables   | plugin, app | Deps Gradle extras (compartidas entre todos los plugins del crate) |
| `[[swift_package]]`  | array de tables   | plugin, app | Paquetes SwiftPM extras                       |
| `[[remote_override]]`| array de tables   | app         | Fuerza un plugin específico al proceso `:remote` |
| `[app]`              | tabla             | app         | Configuración de codegen Kotlin/Swift app-side|

Claves top-level desconocidas se rechazan — espera un error de build
si tipeas mal una sección.

---

## `[plugin]` — forma singular

Usa cuando un crate hospeda exactamente un trait plugin:

```toml
[plugin]
id                  = "acme.telemetry"
client_type         = "::acme_telemetry::TelemetryClient"
default_deployment  = "local"          # o "remote"

[[plugin.gradle]]
group    = "androidx.work"
artifact = "work-runtime-ktx"
version  = "2.9.0"

[[plugin.swift_package]]
url          = "https://github.com/apple/swift-log"
product      = "Logging"
from_version = "1.5.0"
```

### Claves

- **`id`** *(requerido, string)* — identificador del plugin.
  Convención: `<org>.<name>` (`istmo.data_store`, `acme.telemetry`).
- **`client_type`** *(opcional, string)* — path fully-qualified al
  tipo cliente Rust (`::crate::FooClient`). Si está seteado, `emit()`
  extrae el contract del trait correspondiente desde `src/lib.rs`.
  Omítelo para plugins que sólo cargan tipos compartidos.
- **`default_deployment`** *(opcional, `"local"` \| `"remote"`,
  default `"local"`)* — dónde debe correr el backend del plugin por
  default. Apps pueden override vía `[[remote_override]]`.
- **`auto_register`** *(opcional, bool, default `true`)* — si
  `emit_app` debe incluir este plugin en el
  `IstmoPluginRegistry.registerAll(...)` generado. Seteá a `false`
  cuando el `BackendImpl` / `FactoryImpl` del plugin necesita un
  constructor no-estándar y el autor de la app debe registrar el
  dispatcher manualmente. Ver
  [Auto-registración](/es/build-scripts/auto-register/).
- **`[[plugin.gradle]]`** — deps Gradle scopeadas a este plugin.
  Merge con `[[gradle]]` top-level.
- **`[[plugin.swift_package]]`** — deps SwiftPM scopeadas a este plugin.

## `[[plugin]]` — forma array

Usa cuando un solo crate hospeda varios traits plugin:

```toml
[[plugin]]
id          = "istmo.permissions"
client_type = "::istmo_plugins::PermissionsClient"

[[plugin]]
id          = "istmo.notifications"
client_type = "::istmo_plugins::NotificationsClient"

  [[plugin.gradle]]
  group    = "androidx.notifications"
  artifact = "core"
  version  = "1.0.0"

# Aplica a cada [[plugin]] en este crate.
[[gradle]]
group    = "androidx.core"
artifact = "core-ktx"
version  = "1.13.0"
```

## `[[gradle]]`

```toml
[[gradle]]
scope    = "implementation"    # implementation | api | runtimeOnly | compileOnly (default: implementation)
group    = "com.example"
artifact = "widget"
version  = "1.2.3"
```

## `[[swift_package]]`

```toml
[[swift_package]]
url          = "https://github.com/apple/swift-nio"
product      = "NIO"
from_version = "2.62.0"
```

Sólo `.from_version` está soportado hoy; branch y revision van a
aterrizar junto con el primer plugin que los necesite.

---

## `[app]` — codegen app-side

Cada campo es opcional. Defaults en comentarios:

```toml
[app]
android         = true                             # default: true (skipped si no hay ./android)
android_package = "com.myapp.gen"                  # default: "<gradle namespace>.gen"
ios             = true                             # default: true (skipped si no hay ./ios)
ios_app_dir     = "MyApp"                          # default: único subdir ./ios/*
ios_plugins_subdir = "Plugins"                     # default: "Plugins"
auto_register   = true                             # default: true (emitir IstmoPluginRegistry)
```

### `[[app.plugin]]`

Overrides por plugin:

```toml
[[app.plugin]]
id            = "istmo.echo"
role          = "client"                # host | client (default: host)
platforms     = ["android"]             # subset de ["android", "ios"]
auto_register = false                   # skipeá este plugin en el registry generado

[[app.plugin]]
type      = "Notifier"                  # matcheá por nombre de tipo generado en vez de id
role      = "client"
```

Regla de matching: `id` toma precedencia, luego `type`. Cualquiera
matchea por igualdad de string.

## `[[remote_override]]`

Fuerza al cliente de un plugin a rutear por el proceso `:remote` en
Android:

```toml
[[remote_override]]
plugin     = "istmo.push_notifications"
deployment = "remote"                   # local | remote
```

Ver [Proceso remoto](/es/advanced/remote-process/) para la forma del
bridge.

---

## Ejemplo completo (crate plugin)

```toml
[plugin]
id          = "istmo.google_sign_in"
client_type = "::istmo_google_sign_in::SignInClient"

[[gradle]]
group    = "androidx.credentials"
artifact = "credentials"
version  = "1.3.0"

[[gradle]]
group    = "androidx.credentials"
artifact = "credentials-play-services-auth"
version  = "1.3.0"

[[gradle]]
scope    = "api"
group    = "com.google.android.libraries.identity.googleid"
artifact = "googleid"
version  = "1.1.1"

[[swift_package]]
url          = "https://github.com/google/GoogleSignIn-iOS.git"
product      = "GoogleSignIn"
from_version = "7.0.0"
```

## Ejemplo completo (crate app)

```toml
[app]
android_package = "com.myapp.gen"
ios_app_dir     = "MyApp"

[[app.plugin]]
id   = "istmo.echo"
role = "client"

[[remote_override]]
plugin     = "istmo.push_notifications"
deployment = "remote"
```

## Errores

- **Clave top-level desconocida** → `istmo.toml unknown key "widget"`.
  Añádela al schema o remueve la sección.
- **Clave de plugin desconocida** → `istmo.toml unknown key "plugin.foo"`.
- **Tipo incorrecto** → `istmo.toml key "plugin.id" expected string`.
- **Clave requerida faltante** → `istmo.toml missing required key "plugin.id"`.

Todos los errores se reportan en tiempo de `build.rs` — el build para
antes de que se genere código.
