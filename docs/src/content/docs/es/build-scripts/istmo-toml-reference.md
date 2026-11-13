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
| `[app]`              | tabla             | app         | Identidad de la app y configuración del codegen Kotlin/Swift |
| `[[windows_manifest_fragment]]` | array de tables | plugin, app | Entradas del manifiesto de aplicación de Windows |
| `[min_versions]`     | tabla             | plugin, app | Versiones mínimas del SO (requisitos / piso de la app) |

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
  `emit_app` debe incluir este plugin en el `IstmoPluginRegistry`
  generado. Seteá a `false` cuando la construcción del backend varía
  por app (una API key, una view de la app) y el autor de la app debe
  registrar el dispatcher manualmente. Ver
  [Auto-registración](/istmo/es/build-scripts/auto-register/).
- **`android_package`** *(opcional, string)* — package Kotlin donde la
  app genera el dispatcher, la interfaz `<T>Backend`, los tipos y los
  codecs de este plugin. Ponle el package que declaran tus fuentes
  `native/android/`: así nombran los tipos generados sin imports y
  compilan con cualquier `[app] android_package` que use la app.
  Default: el package de codegen de la app (válido para plugins cuyo
  backend escribe la app).
- **`android_backend`** *(opcional, string)* — clase Kotlin
  completamente calificada que el registry le pasa al dispatcher.
  Default: `<T>BackendImpl` / `<T>FactoryImpl` en `android_package`, o
  si no en el package de codegen de la app.
- **`android_backend_arg`** *(opcional, default `"context"`)* — qué
  recibe el constructor de esa clase: `"context"`, `"activity"`
  (`androidx.activity.ComponentActivity`) o una subclase de `Activity`
  completamente calificada (`"androidx.fragment.app.FragmentActivity"`).
- **`ios_backend`** *(opcional, string)* — tipo Swift que el registry
  construye sin argumentos. Default: `<T>BackendImpl` /
  `<T>FactoryImpl`.
- **`[[plugin.gradle]]`** — deps Gradle scopeadas a este plugin.
  Merge con `[[gradle]]` top-level.
- **`[[plugin.swift_package]]`** — deps SwiftPM scopeadas a este plugin.
- **`[plugin.android_service]`** *(opcional, tabla)* — declara un shim
  Kotlin `LifecycleService` que `emit_app` app-side debe generar.
  Campos: `class_name` (requerido), `foreground_service_type`,
  `exported` (default `false`), `permission`, `process`. Ver
  [Services and workers](/istmo/es/writing-plugins/services-workers/) para el
  flujo auto-wire completo.
- **`[plugin.ios_background]`** *(opcional, tabla)* — declara un shim
  iOS `BGTaskScheduler` (o modo background continuous). Campos:
  `class_name` (requerido), `task_identifier`, `kind` (`refresh` |
  `processing` | `continuous`), más keys por kind:
  `interval_minutes` para `refresh`; `requires_power` /
  `requires_network` para `processing`; `continuous_mode` (`audio` |
  `location` | `voip` | `external_accessory` | `bluetooth_central` |
  `bluetooth_peripheral`) para `continuous`.
- **`[plugin.info_plist]`** *(opcional, tabla)* — entradas top-level
  de `Info.plist` que el plugin necesita en iOS (usage descriptions,
  flags de capabilities). Las claves son claves de `Info.plist`; los
  valores son strings o booleanos:
  ```toml
  [plugin.info_plist]
  NSFaceIDUsageDescription = "Authenticate to unlock protected content."
  ```
  `emit_app` mergea las entradas de todos los plugins (gana la primera
  declaración si hay valores en conflicto, con un warning de build),
  aplica los overrides de [`[app.info_plist]`](#appinfo_plist) de la
  app y las escribe en el bloque gestionado de `Info.plist` y en el
  sidecar `Info.plist.background.xml`.

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

## `[app]` — identidad de la app y codegen

Cada campo es opcional. Defaults en comentarios:

```toml
[app]
# Identidad: la aplican el plugin de Gradle `io.github.sergioribera.istmo` y el
# `ios/.istmo/Istmo.xcconfig` generado (ver Proyectos nativos).
id      = "com.myapp"            # applicationId + PRODUCT_BUNDLE_IDENTIFIER (default: lo deciden los proyectos nativos)
name    = "Mi App"               # label del launcher / CFBundleDisplayName (default: nombre del crate)
version = "1.2.0"                # versionName / MARKETING_VERSION (default: package.version de Cargo)
build   = 12                     # versionCode / CURRENT_PROJECT_VERSION, 1..=2100000000 (default: 1)

# Codegen
android         = true           # default: true (skipped si no hay ./android)
android_package = "com.myapp.gen" # default: "<gradle namespace>.gen"
ios             = true           # default: true (skipped si no hay ./ios)
ios_app_dir     = "MyApp"        # default: único subdir ./ios/*
ios_plugins_subdir = "Plugins"   # default: "Plugins"
auto_register   = true           # default: true (registrar plugins en IstmoPluginRegistry)
rust_entry      = true           # default: detectado (`#[istmo::mobile_app]` en src/); emite IstmoApp.run()

# Ícono de launcher (ver Proyectos nativos). Todas las claves son opcionales.
[app.icon]
image   = "assets/icon.png"      # ícono de iOS + mipmaps legacy de Android (default: ninguno)
desktop = "assets/tile.png"      # ícono de ventana + del .exe de Windows (default: image)

[app.icon.android]               # ícono adaptativo, API 26+ (default: ninguno)
foreground = "assets/fg.xml"     # obligatoria: vector drawable .xml o .png
background = "#EFE6D4"           # obligatoria: #RRGGBB / #AARRGGBB, .xml o .png
monochrome = "assets/mono.xml"   # ícono temático, Android 13+ (default: ninguno)
```

`android_package` solo recibe los contratos cuyo plugin no declara
`[plugin] android_package` (los plugins propios de la app y los
contratos built-in). Los plugins oficiales se generan en
`dev.istmo.plugins.<nombre>`, junto a sus fuentes Kotlin, así que el
código escrito a mano en la app que nombra uno de sus tipos generados
(un dispatcher registrado a mano, un codec) lo importa desde ahí, p. ej.
`import dev.istmo.plugins.share.ShareDispatcher`. Ya no hace falta fijar
`android_package = "dev.istmo.runtime"` para que compilen las fuentes de
los plugins; las apps que lo mantienen siguen compilando.

`id` tiene que ser un identificador reverse-DNS (`com.example.app`: dos
o más segmentos separados por puntos, de letras ASCII, dígitos y `_`,
cada uno empezando con letra). `[min_versions] android` / `ios` son el
`minSdk` y el deployment target de la app. Cómo llega cada valor a
Gradle y Xcode está en
[Proyectos nativos](/istmo/es/build-scripts/native-projects/).

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

### `[app.info_plist]`

Sobrescribe (o agrega) entradas de `Info.plist` por encima de las que
declaran los plugins con `[plugin.info_plist]` — típicamente para
localizar una usage description:

```toml
[app.info_plist]
NSFaceIDUsageDescription = "Usá Face ID para abrir tu bóveda."
```

Las entradas se escriben entre los marcadores gestionados del
`Info.plist` de la app, compartidos con `[plugin.ios_background]`:

```xml
<dict>
    <!-- istmo:background:start -->
    <!-- istmo:background:end -->
</dict>
```

Sin los marcadores, copiá las entradas a mano desde el sidecar
generado `Info.plist.background.xml`.

### `[[app.document_type]]`

Tipos de archivo que abre la app. Una sola declaración alimenta todas las plataformas:

```toml
[app]
single_instance = true          # escritorio; por defecto: true si hay algún document_type

[[app.document_type]]
name       = "PDF document"     # obligatorio: nombre que muestra el sistema
id         = "com.adobe.pdf"    # UTI (Apple) / base del ProgID (Windows); obligatorio si exported
extensions = ["pdf"]            # sin el punto
mime_types = ["application/pdf"]
role       = "viewer"           # viewer | editor (por defecto: viewer)
rank       = "alternate"        # owner | default | alternate (por defecto: alternate)

[[app.document_type]]
name        = "Notes document"
id          = "com.example.notes.document"
extensions  = ["note"]
mime_types  = ["application/x-note"]
role        = "editor"
rank        = "owner"           # solo en tipos exportados
exported    = true              # la app define este tipo (por defecto: false)
conforms_to = ["public.data"]   # tipos padre en Apple (por defecto: ["public.data"])
```

Cada entrada necesita `extensions` o `mime_types`.

- **Android** — filtros `VIEW` y `SEND` por tipo MIME, más un filtro
  `VIEW` por `pathPattern` para las extensiones de los tipos exportados.
  Pertenecen a una sola actividad, así que van entre marcadores que ponés
  dentro del `<activity>` que recibe documentos:

  ```xml
  <activity android:name=".MainActivity" ...>
      <!-- istmo:documents:start -->
      <!-- istmo:documents:end -->
  </activity>
  ```

  Sin los marcadores, copialos de `app/src/main/AndroidManifest.documents.xml`.
- **iOS** — `CFBundleDocumentTypes`, `UTExportedTypeDeclarations`,
  `UTImportedTypeDeclarations` (los tipos del sistema como
  `com.adobe.pdf` no se vuelven a importar) y, si algún tipo es
  `editor`, `LSSupportsOpeningDocumentsInPlace`, entre los marcadores
  `istmo:background` del `Info.plist`.
- **Escritorio** — las herramientas de empaquetado leen la misma tabla
  (por ejemplo `fileAssociations` de nix-bundle-app). En ejecución,
  `istmo::plugins::DocumentsPublisher` publica los documentos de la línea
  de comandos y, con `single_instance`, reenvía los de un arranque
  posterior a la instancia que ya corre (socket Unix protegido con
  `flock` en Linux y macOS, named pipe en Windows). El build emite
  `ISTMO_SINGLE_INSTANCE`; `istmo::single_instance!()` lo convierte en la
  clave de instancia.

Los documentos llegan por el plugin `istmo.documents`
(`Documents::stream()`); Android e iOS publican en el mismo canal desde
sus manejadores de intents / URLs.

## `[[remote_override]]`

Fuerza al cliente de un plugin a rutear por el proceso `:remote` en
Android:

```toml
[[remote_override]]
plugin     = "istmo.push_notifications"
deployment = "remote"                   # local | remote
```

Ver [Proceso remoto](/istmo/es/advanced/remote-process/) para la forma del
bridge.

---

## `[min_versions]`

Versión más antigua del SO que soporta el crate. Todas las claves son
opcionales.

| Clave | Tipo | Significado |
|---|---|---|
| `android` | entero | API level de Android (`minSdk`). |
| `ios` | string | Deployment target de iOS, p. ej. `"15.0"`. |
| `macos` | string | Deployment target de macOS, p. ej. `"11.0"`. |
| `windows` | string | Build de Windows, p. ej. `"10.0.17763"`. |

```toml
[min_versions]
android = 24
ios     = "15.0"
macos   = "11.0"
windows = "10.0.17763"
```

En un crate **plugin** son requisitos. En un crate **app** son el piso
al que apunta la app. `istmo_build::emit()` compara cada plugin contra
el piso de la app para el target que se compila y falla el build si un
plugin necesita un SO más nuevo. El piso sale del `[min_versions]` de
la app si existe; si no, se detecta:

| Target | Se detecta desde |
|---|---|
| Android | `minSdk` en `android/app/build.gradle(.kts)` |
| iOS | `$IPHONEOS_DEPLOYMENT_TARGET` |
| macOS | `$MACOSX_DEPLOYMENT_TARGET` |
| Windows | sólo el `[min_versions]` de la app |

Las herramientas de cada plataforma repiten la comprobación: el plugin
de Gradle `io.github.sergioribera.istmo` compara `android` contra el `minSdk` resuelto de cada
variante, y el `istmo-plugins.yml` generado agrega un script de
pre-build en Xcode que compara `ios` contra
`IPHONEOS_DEPLOYMENT_TARGET`.

## Fragmentos de manifiestos nativos

Los plugins que necesitan entradas en los manifiestos de la app los
incluyen como archivos junto a sus fuentes nativas; no van en
`istmo.toml`.

| Archivo | Se mergea en | Lo hace |
|---|---|---|
| `native/android/AndroidManifest.xml` | el manifest de cada variante | plugin de Gradle `io.github.sergioribera.istmo` |
| `native/android/res/` | recursos de la app | plugin de Gradle `io.github.sergioribera.istmo` |
| `native/ios/Info.plist.fragment` | `Info.plist` de la app | `istmo-build` |
| `native/ios/App.entitlements.fragment` | `.entitlements` de la app | `istmo-build` |

El fragmento de Android es un manifest normal de librería
(`<manifest>` con hijos de `<application>`, `<queries>`,
`<uses-permission>`); los placeholders `${applicationId}` funcionan.

Los fragmentos de Apple son property lists cuyo valor raíz es un
`<dict>`. `istmo-build` los mergea (dicts recursivamente, arrays
concatenados, el primer plugin gana en conflictos escalares y la app
siempre gana sobre los plugins) y escribe el resultado entre marcadores
dentro del `<dict>` de la app:

```xml
<!-- istmo:plugins:start -->
<!-- istmo:plugins:end -->
```

Sin marcadores, las claves se escriben en un archivo aparte
(`Info.plist.plugins.xml`, `<App>.entitlements.plugins.xml`) para
copiarlas a mano.

### `[[windows_manifest_fragment]]`

Las entradas del manifiesto de aplicación de Windows sí van en
`istmo.toml` (de plugin o de app). Cada `xml` es hijo directo de
`<assembly>`:

```toml
[[windows_manifest_fragment]]
name = "dpi_aware"
xml  = """
<application xmlns="urn:schemas-microsoft-com:asm.v3">
  <windowsSettings>
    <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
  </windowsSettings>
</application>
"""
```

Cuando la app compila para Windows, `istmo-build` deja un fragmento por
`name` (gana la primera declaración), los envuelve en un `<assembly>` y
embebe el resultado: `/MANIFEST:EMBED /MANIFESTINPUT:` en MSVC, un
recurso `RT_MANIFEST` junto al ícono del `.exe` en GNU.

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
id              = "com.myapp"
name            = "Mi App"
build           = 3
android_package = "com.myapp.gen"
ios_app_dir     = "MyApp"

[app.icon]
image = "assets/icon.png"

[min_versions]
android = 24
ios     = "15.0"

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
- **Valor inválido** → `istmo.toml key "app.id" = "demo" (expected a
  reverse-DNS id such as com.example.app)`.

Todos los errores se reportan en tiempo de `build.rs` — el build para
antes de que se genere código. La sección `[app]` se valida igual de
estricto que el resto: una clave mal escrita como `andorid_package` es
un error, no un setting ignorado en silencio.
