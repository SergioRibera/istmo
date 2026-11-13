---
title: Referencia de istmo.toml
description: Cada clave, cada default, cada valor válido.
sidebar:
  order: 2
---

Cada crate de plugin y cada crate de app puede tener un `istmo.toml`
junto a su `Cargo.toml`. Todas las secciones son opcionales; un archivo
vacío (o ausente) es válido. El parseo es estricto: una clave
desconocida, un valor del tipo equivocado o un valor inválido hacen
fallar el build (ver [Errores](#errores)).

## Tablas de primer nivel

| Clave                           | Tipo            | Dónde       | Propósito                                            |
| ------------------------------- | --------------- | ----------- | ---------------------------------------------------- |
| `[plugin]` / `[[plugin]]`       | tabla / array   | plugin      | Declara uno o más traits de plugin                   |
| `[[gradle]]`                    | array de tablas | plugin, app | Dependencias Gradle (compartidas por todos los plugins del crate) |
| `[[swift_package]]`             | array de tablas | plugin, app | Paquetes SwiftPM                                     |
| `[[assets]]`                    | array de tablas | plugin, app | Archivos empaquetados con la app                     |
| `[app]`                         | tabla           | app         | Identidad de la app y configuración del codegen Kotlin/Swift |
| `[[remote_override]]`           | array de tablas | app         | Mueve un plugin al (o fuera del) proceso `:remote`   |
| `[min_versions]`                | tabla           | plugin, app | Versiones mínimas del SO (requisitos / piso de la app) |
| `[[windows_manifest_fragment]]` | array de tablas | plugin, app | Entradas del manifest de aplicación de Windows       |

---

## `[plugin]` — forma singular

Úsala cuando un crate aloja exactamente un trait de plugin:

```toml
[plugin]
id                  = "acme.telemetry"
client_type         = "::acme_telemetry::TelemetryClient"
default_deployment  = "local"          # o "remote"
android_package     = "com.acme.telemetry"

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

- **`id`** *(obligatoria, string)* — identificador del plugin, el mismo
  string que el `#[istmo::plugin(name = "…")]` del trait. Sólo letras
  ASCII, dígitos, `.`, `_` y `-`. Convención: `<org>.<nombre>`
  (`istmo.data_store`, `acme.telemetry`).
- **`client_type`** *(opcional, string)* — path del tipo client en Rust
  (`::my_crate::FooClient`); debe parsear como path de Rust y su último
  segmento debe terminar en `Client`. Si está, `emit()` extrae el
  contrato del trait `Foo` de `src/lib.rs` y el auto-wiring agrega el
  client a `istmo::runtime!`. Omítela en crates que sólo llevan tipos
  compartidos o sólo un shim de servicio/background.
- **`default_deployment`** *(opcional, `"local"` \| `"remote"`, default
  `"local"`)* — dónde corre el backend del plugin por defecto. Las apps
  pueden sobrescribirlo con `[[remote_override]]`.
- **`auto_register`** *(opcional, bool, default `true`)* — si `emit()`
  incluye este plugin en el `IstmoPluginRegistry` generado. Ponlo en
  `false` cuando la construcción del backend varía por app (una API key,
  una vista que es de la app) y el autor de la app debe registrar el
  dispatcher a mano. Ver [Auto-registro](/istmo/es/build-scripts/auto-register/).
- **`android_package`** *(opcional, string)* — paquete Kotlin en el que
  la app genera el dispatcher, la interfaz `<T>Backend`, los tipos y los
  codecs de este plugin (un paquete Kotlin con puntos, p. ej.
  `dev.istmo.plugins.share`). Ponle el paquete que declaran tus fuentes
  `native/android/`: así nombran los tipos generados sin imports y
  compilan con cualquier `[app] android_package` que use la app.
  Default: el paquete de codegen de la app (sirve para plugins cuyo
  backend escribe la app).
- **`android_backend`** *(opcional, string)* — clase Kotlin
  completamente calificada que el registry le pasa al dispatcher.
  Default: `<T>BackendImpl` / `<T>FactoryImpl` en `android_package`, o
  si no en el paquete de codegen de la app.
- **`android_backend_arg`** *(opcional, default `"context"`)* — qué
  recibe el constructor de esa clase: `"context"`, `"activity"`
  (`androidx.activity.ComponentActivity`) o una subclase de `Activity`
  completamente calificada (`"androidx.fragment.app.FragmentActivity"`).
- **`ios_backend`** *(opcional, string)* — tipo Swift que el registry
  construye sin argumentos. Default: `<T>BackendImpl` / `<T>FactoryImpl`.
- **`android_permissions`** *(opcional, array)* — permisos de Android
  que necesita el plugin. Cada entrada es un string o una tabla inline
  con un tope de API:
  ```toml
  android_permissions = [
      "android.permission.CAMERA",
      { name = "android.permission.WRITE_EXTERNAL_STORAGE", max_sdk_version = 28 },
  ]
  ```
  Los nombres fuera de `android.permission.*`, `com.android.vending.*` y
  `com.google.android.*` producen un warning. La app escribe los
  permisos de los plugins que conecta en
  `android/.istmo/istmo-permissions.xml`, que el plugin de Gradle
  fusiona en el manifest de cada variante.
- **`[[plugin.gradle]]`** — dependencias Gradle de este plugin, con las
  mismas claves que [`[[gradle]]`](#gradle). Se suman a cualquier
  `[[gradle]]` de primer nivel.
- **`[[plugin.swift_package]]`** — dependencias SwiftPM de este plugin,
  con las mismas claves que [`[[swift_package]]`](#swift_package).
- **`[plugin.android_service]`** *(opcional, tabla)* — declara un shim
  Kotlin `LifecycleService` que genera el build de la app. Campos:
  `class_name` (obligatorio), `foreground_service_type`, `exported`
  (default `false`), `permission`, `process`. Ver
  [Services y workers](/istmo/es/writing-plugins/services-workers/) para
  el flujo completo.
- **`[plugin.ios_background]`** *(opcional, tabla)* — declara un shim
  iOS de `BGTaskScheduler` (o un modo de background continuo). Campos:
  `class_name` (obligatorio), `kind` (obligatorio: `refresh` |
  `processing` | `continuous`), `task_identifier`, más claves según el
  tipo: `interval_minutes` (obligatoria) para `refresh`;
  `requires_power` / `requires_network` (default `false`) para
  `processing`; `continuous_mode` (obligatoria: `audio` | `location` |
  `voip` | `external_accessory` | `bluetooth_central` |
  `bluetooth_peripheral`) para `continuous`.
- **`[plugin.info_plist]`** *(opcional, tabla)* — entradas de primer
  nivel de `Info.plist` que el plugin necesita en iOS (descripciones de
  uso, flags de capacidades). Las claves son claves de `Info.plist`; los
  valores son strings o booleanos:
  ```toml
  [plugin.info_plist]
  NSFaceIDUsageDescription = "Autentícate para desbloquear contenido protegido."
  ```
  La app fusiona las entradas de los plugins que conecta (gana la
  primera declaración ante valores en conflicto, con un warning), aplica
  sus propios overrides de [`[app.info_plist]`](#appinfo_plist) y los
  escribe en el bloque gestionado de `Info.plist` y en el sidecar
  `Info.plist.background.xml`.

Los permisos, las entradas de `Info.plist` y los shims de servicio y de
background sólo llegan a la app para los plugins que conecta con
`istmo::runtime!` (más los plugins sin `client_type`, que no se pueden
listar ahí).

## `[[plugin]]` — forma array

Úsala cuando un solo crate aloja varios traits de plugin. Cada entrada
acepta las mismas claves que `[plugin]`; dos entradas no pueden
compartir `id`:

```toml
[[plugin]]
id          = "acme.permissions"
client_type = "::acme_platform::PermissionsClient"

[[plugin]]
id          = "acme.notifications"
client_type = "::acme_platform::NotificationsClient"

  [[plugin.gradle]]
  group    = "androidx.core"
  artifact = "core-ktx"
  version  = "1.13.1"

# Aplica a cada [[plugin]] de este crate.
[[gradle]]
group    = "androidx.work"
artifact = "work-runtime-ktx"
version  = "2.9.0"
```

## `[[gradle]]`

```toml
[[gradle]]
scope    = "implementation"    # implementation | api | runtimeOnly | compileOnly (default: implementation)
group    = "com.example"       # obligatoria
artifact = "widget"            # obligatoria
version  = "1.2.3"             # obligatoria
```

La app junta las entradas `[[gradle]]` de cada plugin con las suyas.
Cuando dos crates piden el mismo artefacto en el mismo scope con
versiones distintas, gana la más alta y el build imprime un warning. El
plugin de Gradle `io.github.sergioribera.istmo` agrega el resultado al
módulo de la app.

## `[[swift_package]]`

```toml
[[swift_package]]
url          = "https://github.com/apple/swift-nio"   # obligatoria
product      = "NIO"                                  # obligatoria
from_version = "2.62.0"                               # obligatoria
```

Sólo se soporta `from_version` ("hasta la siguiente major"). Las
entradas se combinan igual que las de Gradle (gana el `from_version` más
alto) y se pueden leer desde un build script con
`istmo_build::collect_dep_native_deps().swift_entries()`. El
`istmo-plugins.yml` generado no declara paquetes, así que agrega cada
uno a tu proyecto de Xcode (la sección `packages:` de `project.yml`, o
File → Add Package Dependencies).

## `[[assets]]`

Archivos empaquetados con la app, legibles en tiempo de ejecución con
`istmo::assets`:

```toml
[[assets]]
path = "assets/logo.png"       # un archivo

[[assets]]
path = "assets/fonts"          # un directorio, recorrido recursivamente

[[assets]]
path = "assets/i18n/*.json"    # un glob
```

- **`path`** *(obligatoria, string no vacío)* — relativo a la raíz del
  crate, y debe quedarse dentro de ella. Un path que no existe, o un
  glob que no coincide con nada, hace fallar el build.

La clave de búsqueda quita un `assets/` inicial: la entrada
`assets/logo.png` se lee con `istmo::assets::read("logo.png")`. Los
assets propios de la app quedan en la raíz del bundle; los de un plugin
quedan en `plugin-<id>/` y se leen con
`istmo::assets::plugin("<id>").read("…")`.

Dónde terminan:

- **Android** — `android/app/src/main/assets/` (los de plugins en
  `plugin-<id>/`), empaquetados por el merger de assets de Gradle.
- **iOS** — `ios/.istmo/Assets/`, enviado como una sola folder reference
  para que los archivos queden en `<bundle>/Assets/…` (ver
  [Proyectos nativos](/istmo/es/build-scripts/native-projects/#ios)).
- **Escritorio** — se preparan bajo el `OUT_DIR` del build y se graban
  en el binario como `ISTMO_APP_ASSETS_DIR`, salvo que `[app] assets_dir`
  indique otro directorio. La variable de entorno `ISTMO_ASSETS_DIR`
  sobrescribe ambos en tiempo de ejecución.

Los assets que ya no están declarados se eliminan de estos árboles en el
siguiente build.

---

## `[app]`

Identidad de la app y codegen. Todas las claves son opcionales; los
defaults van en los comentarios:

```toml
[app]
# Identidad: la aplican el plugin de Gradle `io.github.sergioribera.istmo` y el
# `ios/.istmo/Istmo.xcconfig` generado (ver Proyectos nativos).
id      = "com.myapp"            # applicationId + PRODUCT_BUNDLE_IDENTIFIER (default: lo deciden los proyectos nativos)
name    = "My App"               # etiqueta del launcher / CFBundleDisplayName (default: nombre del paquete Cargo)
version = "1.2.0"                # versionName / MARKETING_VERSION (default: package.version de Cargo)
build   = 12                     # versionCode / CURRENT_PROJECT_VERSION, 1..=2100000000 (default: 1)

# Codegen
android            = true             # default: true (se salta si no hay ./android)
android_package    = "com.myapp.gen"  # default: "<namespace de gradle>.gen"
ios                = true             # default: true (se salta si no hay ./ios)
ios_app_dir        = "MyApp"          # default: el único subdirectorio de app en ./ios/*
ios_plugins_subdir = "Plugins"        # default: "Plugins"
auto_register      = true             # default: true (registra plugins en IstmoPluginRegistry)
rust_entry         = true             # default: detectado (`#[istmo::mobile_app]` en src/); emite IstmoApp.run()

# Extras de plataforma
android_permissions = ["android.permission.INTERNET"]  # mismo formato que [plugin] android_permissions
assets_dir          = "assets"        # raíz de assets en escritorio (default: el dir preparado de [[assets]])
single_instance     = true            # escritorio; default: true si hay algún [[app.document_type]]

# Ícono del launcher (ver Proyectos nativos). Todas las claves son opcionales.
[app.icon]
image   = "assets/icon.png"      # ícono de app iOS + mipmaps legacy de Android (default: ninguno)
desktop = "assets/tile.png"      # ícono de ventana + ícono del .exe en Windows (default: image)

[app.icon.android]               # ícono adaptativo, API 26+ (default: ninguno)
foreground = "assets/fg.xml"     # obligatoria: vector drawable .xml o .png
background = "#EFE6D4"           # obligatoria: #RRGGBB / #AARRGGBB, .xml o .png
monochrome = "assets/mono.xml"   # ícono temático, Android 13+ (default: ninguno)
```

- `id` debe ser un identificador reverse-DNS (`com.example.app`: dos o
  más segmentos separados por puntos, de letras ASCII, dígitos y `_`,
  cada uno empezando con letra).
- `android_package` sólo contiene los contratos cuyo plugin no declara
  `[plugin] android_package` (los plugins propios de la app y los
  contratos built-in). Los plugins oficiales generan en
  `dev.istmo.plugins.<nombre>` junto a sus fuentes Kotlin, así que el
  código escrito a mano en la app que nombra uno de sus tipos generados
  (un dispatcher registrado a mano, un codec) lo importa desde ahí, p.
  ej. `import dev.istmo.plugins.share.ShareDispatcher`.
- `android_permissions` agrega los permisos que necesita el propio
  código de la app, además de los de los plugins conectados.
- `assets_dir` se resuelve contra la raíz del crate si es relativo.
- `[min_versions] android` / `ios` son el `minSdk` y el deployment
  target de la app.

Cómo llega cada valor a Gradle y Xcode se describe en
[Proyectos nativos](/istmo/es/build-scripts/native-projects/).

### `[[app.plugin]]`

Overrides por plugin (también vale una sola tabla `[app.plugin]`):

```toml
[[app.plugin]]
id            = "istmo.echo"
role          = "client"                # host | client (default: host)
platforms     = ["android"]             # subconjunto de ["android", "ios"] (default: todas las activas)
auto_register = false                   # omite este plugin en el registry generado

[[app.plugin]]
type      = "Notifier"                  # hace match por nombre de tipo generado en vez de id
role      = "client"
```

Cada entrada necesita `id` o `type`; si están las dos, se usa `id`. Un
contrato se busca primero por su id de plugin y luego por su nombre de
tipo.

### `[app.info_plist]`

Sobrescribe (o agrega) entradas de `Info.plist` encima de las que
declaran los plugins con `[plugin.info_plist]` — típicamente para
localizar una descripción de uso:

```toml
[app.info_plist]
NSFaceIDUsageDescription = "Usa Face ID para abrir tu bóveda."
```

Las entradas quedan entre los marcadores gestionados del `Info.plist` de
la app, compartidos con `[plugin.ios_background]`:

```xml
<dict>
    <!-- istmo:background:start -->
    <!-- istmo:background:end -->
</dict>
```

Sin los marcadores, copia las entradas a mano desde el sidecar generado
`Info.plist.background.xml`.

### `[[app.document_type]]`

Tipos de archivo que abre la app. Una declaración alimenta a todas las
plataformas:

```toml
[app]
single_instance = true          # escritorio; default: true si se declara algún document_type

[[app.document_type]]
name       = "PDF document"     # obligatoria: nombre que muestra el sistema
id         = "com.adobe.pdf"    # UTI (Apple) / base del ProgID (Windows); obligatoria si exported
extensions = ["pdf"]            # sin el punto
mime_types = ["application/pdf"]
role       = "viewer"           # viewer | editor (default: viewer)
rank       = "alternate"        # owner | default | alternate (default: alternate)

[[app.document_type]]
name        = "Notes document"
id          = "com.example.notes.document"
extensions  = ["note"]
mime_types  = ["application/x-note"]
role        = "editor"
rank        = "owner"           # sólo en tipos exportados
exported    = true              # la app define este tipo (default: false)
conforms_to = ["public.data"]   # tipos padre de Apple (default: ["public.data"])
```

Cada entrada necesita `extensions` o `mime_types`; una entrada exportada
necesita `id` y `extensions`. Las extensiones se escriben sin el punto
inicial, los tipos MIME como `tipo/subtipo`.

- **Android** — intent filters `VIEW` y `SEND` por tipo MIME, más un
  filtro `VIEW` por path pattern para las extensiones de los tipos
  exportados. Pertenecen a una activity, así que quedan entre
  marcadores que pones dentro de la `<activity>` que recibe documentos:

  ```xml
  <activity android:name="dev.istmo.runtime.IstmoGameActivity" ...>
      <!-- istmo:documents:start -->
      <!-- istmo:documents:end -->
  </activity>
  ```

  Sin los marcadores, cópialos desde `app/src/main/AndroidManifest.documents.xml`.
- **iOS** — `CFBundleDocumentTypes`, `UTExportedTypeDeclarations`,
  `UTImportedTypeDeclarations` (los tipos del sistema como
  `com.adobe.pdf` no se reimportan) y, cuando un tipo es `editor`,
  `LSSupportsOpeningDocumentsInPlace`, escritos entre los marcadores
  `istmo:background` de `Info.plist`.
- **Escritorio** — las herramientas de empaquetado leen la misma tabla
  (por ejemplo `fileAssociations` de nix-bundle-app). En tiempo de
  ejecución, `istmo::plugins::DocumentsPublisher` publica los documentos
  de la línea de comandos y, con `single_instance`, reenvía los
  documentos de un lanzamiento posterior a la instancia en ejecución
  (socket Unix protegido por un `flock` en Linux y macOS, named pipe en
  Windows). El build emite `ISTMO_SINGLE_INSTANCE`;
  `istmo::single_instance!()` lo convierte en la clave de instancia.

Los documentos abiertos llegan por el plugin `istmo.documents`
(`Documents::stream()`); los shells de Android e iOS publican en el
mismo canal desde sus handlers de intents / URLs.

## `[[remote_override]]`

Sobrescribe el `default_deployment` de un plugin desde la app:

```toml
[[remote_override]]
plugin     = "acme.large_model"       # obligatoria: id del plugin
deployment = "remote"                 # obligatoria: local | remote
```

El auto-wiring declara entonces el client del plugin como remoto (o
local) en `istmo::runtime!`. Ver
[Proceso remoto](/istmo/es/advanced/remote-process/) para la forma del
puente.

---

## `[min_versions]`

La versión más antigua del SO que soporta el crate. Todas las claves son
opcionales.

| Clave | Tipo | Significado |
|---|---|---|
| `android` | entero | Nivel de API de Android (`minSdk`). |
| `ios` | string | Deployment target de iOS, p. ej. `"15.0"`. |
| `macos` | string | Deployment target de macOS, p. ej. `"11.0"`. |
| `windows` | string | Build de Windows, p. ej. `"10.0.17763"`. |

Las versiones son `major[.minor[.patch]]`; también se acepta un entero
solo (`ios = 15`).

```toml
[min_versions]
android = 24
ios     = "15.0"
macos   = "11.0"
windows = "10.0.17763"
```

En un crate de **plugin** son requisitos. En un crate de **app** son el
piso al que apunta la app. `istmo_build::emit()` compara cada plugin
dependencia con el piso de la app para el target que se compila y hace
fallar el build, listando cada plugin culpable, cuando alguno necesita un
SO más nuevo. El piso se toma del `[min_versions]` de la propia app si
existe; si no, se detecta:

| Target | Detectado desde |
|---|---|
| Android | `minSdk` en `android/app/build.gradle(.kts)` |
| iOS | `$IPHONEOS_DEPLOYMENT_TARGET` |
| macOS | `$MACOSX_DEPLOYMENT_TARGET` |
| Windows | sólo el `[min_versions]` de la app |

Sin piso con el que comparar, la comprobación se salta. Los demás
targets (Linux, wasm, …) nunca se comprueban.

Las herramientas de plataforma repiten la comprobación por su cuenta: el
plugin de Gradle `io.github.sergioribera.istmo` compara `android` con el
`minSdk` resuelto de cada variante, y el `istmo-plugins.yml` generado
agrega un script de pre-build en Xcode que compara el `ios` más estricto
de los plugins con `IPHONEOS_DEPLOYMENT_TARGET`.

## Fragmentos de manifiestos nativos

Los plugins que necesitan entradas en los manifests de plataforma de la
app, más allá de permisos y claves de `Info.plist`, las envían como
archivos junto a sus fuentes nativas; nada va en `istmo.toml`.

| Archivo | Se fusiona en | Lo hace |
|---|---|---|
| `native/android/AndroidManifest.xml` | el manifest de cada variante | plugin de Gradle `io.github.sergioribera.istmo` |
| `native/android/res/` | recursos de la app | plugin de Gradle `io.github.sergioribera.istmo` |
| `native/ios/Info.plist.fragment` | `Info.plist` de la app | `istmo-build` |
| `native/ios/App.entitlements.fragment` | `.entitlements` de la app | `istmo-build` |

El fragmento de Android es un manifest normal de librería (`<manifest>`
con hijos de `<application>`, `<queries>`, `<uses-permission>`); los
placeholders `${applicationId}` funcionan.

Los fragmentos de Apple son property lists cuyo valor raíz es un
`<dict>`. `istmo-build` los fusiona (dicts recursivamente, arrays
concatenados, gana el primer plugin en conflictos de escalares, la app
siempre gana sobre los plugins) y escribe el resultado entre marcadores
puestos dentro del `<dict>` de la app:

```xml
<!-- istmo:plugins:start -->
<!-- istmo:plugins:end -->
```

Sin marcadores, las claves fusionadas se escriben en un sidecar
(`Info.plist.plugins.xml`, `<App>.entitlements.plugins.xml`) para
copiarlas a mano.

### `[[windows_manifest_fragment]]`

Las entradas del manifest de aplicación de Windows sí viven en
`istmo.toml` (de plugin o de app). Las dos claves son obligatorias; cada
`xml` es un hijo directo de `<assembly>`:

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

Cuando la app compila para Windows, `istmo-build` se queda con un
fragmento por `name` (gana la primera declaración), los envuelve en un
`<assembly>` y embebe el resultado: `/MANIFEST:EMBED /MANIFESTINPUT:` en
MSVC, un recurso `RT_MANIFEST` junto al ícono del `.exe` en GNU.

## Ejemplo completo (crate de plugin)

```toml
[plugin]
id                  = "istmo.google_sign_in"
client_type         = "::istmo_google_sign_in::SignInClient"
android_package     = "dev.istmo.plugins.googlesignin"
android_backend     = "dev.istmo.plugins.googlesignin.GoogleSignInFactoryImpl"
android_backend_arg = "activity"
ios_backend         = "GoogleSignInFactoryImpl"

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
from_version = "7.1.0"

[min_versions]
android = 21
```

## Ejemplo completo (crate de app)

```toml
[app]
id              = "com.myapp"
name            = "My App"
build           = 3
android_package = "com.myapp.gen"
ios_app_dir     = "MyApp"

[app.icon]
image = "assets/icon.png"

[min_versions]
android = 24
ios     = "15.0"

[[assets]]
path = "assets/data"

[[app.plugin]]
id   = "istmo.echo"
role = "client"

[[remote_override]]
plugin     = "acme.large_model"
deployment = "remote"
```

## Errores

Cada error nombra la clave y detiene el build en `build.rs`, antes de
generar código:

- **Clave desconocida** → ``istmo.toml unknown key `widget` `` (las
  claves anidadas van completas: `` `plugin.foo` ``,
  `` `app.andorid_package` ``). La sección `[app]` se valida con el
  mismo rigor que el resto.
- **Tipo equivocado** → ``istmo.toml key `plugin.id` expected string``.
- **Falta una clave obligatoria** → ``istmo.toml missing required key `plugin.id` ``.
- **Valor inválido** → ``istmo.toml key `app.id` = `demo` (expected a
  reverse-DNS id such as `com.example.app`)``. La misma forma reporta un
  id de plugin, `client_type`, paquete Kotlin, número de build, id de
  `[[plugin]]` duplicado, capa de ícono o tipo de documento inválidos.
- **Valor de enum desconocido** → para `deployment` /
  `default_deployment`, el `scope` de Gradle, `ios_background.kind` y
  `continuous_mode`, el mensaje lista los valores aceptados.
- **Versión inválida** → ``istmo.toml key `min_versions.ios` = `15.x`
  (expected an API level integer for `android`, or a
  `major[.minor[.patch]]` string)``.
- **Sintaxis TOML** → `parsing istmo.toml: …` con la ubicación que da el
  parser.
