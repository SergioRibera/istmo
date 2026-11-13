---
title: istmo.toml reference
description: Every key, every default, every valid value.
sidebar:
  order: 2
---

Every plugin crate and every app crate can have an `istmo.toml` next to
its `Cargo.toml`. Every section is optional; an empty (or missing) file
is legal. Parsing is strict: an unknown key, a value of the wrong type
or an invalid value fails the build (see [Errors](#errors)).

## Top-level tables

| Key                             | Type            | Where       | Purpose                                              |
| ------------------------------- | --------------- | ----------- | ---------------------------------------------------- |
| `[plugin]` / `[[plugin]]`       | table / array   | plugin      | Declare one or more plugin traits                    |
| `[[gradle]]`                    | array of tables | plugin, app | Gradle dependencies (shared by every plugin in the crate) |
| `[[swift_package]]`             | array of tables | plugin, app | SwiftPM packages                                     |
| `[[assets]]`                    | array of tables | plugin, app | Files bundled with the app                           |
| `[app]`                         | table           | app         | App identity and Kotlin/Swift codegen configuration  |
| `[[remote_override]]`           | array of tables | app         | Move a plugin to (or out of) the `:remote` process   |
| `[min_versions]`                | table           | plugin, app | Minimum OS versions (requirements / app floor)       |
| `[[windows_manifest_fragment]]` | array of tables | plugin, app | Entries of the Windows application manifest          |

---

## `[plugin]` — single form

Use when a crate hosts exactly one plugin trait:

```toml
[plugin]
id                  = "acme.telemetry"
client_type         = "::acme_telemetry::TelemetryClient"
default_deployment  = "local"          # or "remote"
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

### Keys

- **`id`** *(required, string)* — plugin identifier, the same string as
  the trait's `#[istmo::plugin(name = "…")]`. ASCII letters, digits,
  `.`, `_` and `-` only. Convention: `<org>.<name>` (`istmo.data_store`,
  `acme.telemetry`).
- **`client_type`** *(optional, string)* — path to the Rust client type
  (`::my_crate::FooClient`); it must parse as a Rust path and its last
  segment must end in `Client`. If set, `emit()` extracts the `Foo`
  trait's contract from `src/lib.rs`, and auto-wiring adds the client to
  `istmo::runtime!`. Omit it for crates that carry only shared types or
  only a service/background shim.
- **`default_deployment`** *(optional, `"local"` \| `"remote"`,
  default `"local"`)* — where the plugin's backend runs by default.
  Apps can override it via `[[remote_override]]`.
- **`auto_register`** *(optional, bool, default `true`)* — whether
  `emit()` includes this plugin in the generated
  `IstmoPluginRegistry`. Set to `false` when the backend's construction
  varies per app (an API key, a view the app owns) and the app author
  must register the dispatcher manually. See
  [Auto-registration](/istmo/build-scripts/auto-register/).
- **`android_package`** *(optional, string)* — Kotlin package the app
  generates this plugin's dispatcher, `<T>Backend` interface, types and
  codecs into (a dotted Kotlin package, e.g. `dev.istmo.plugins.share`).
  Set it to the package your `native/android/` sources declare: they
  then name the generated types without imports and compile whatever
  `[app] android_package` the consuming app uses. Default: the app's
  codegen package (fine for plugins whose backend the app writes).
- **`android_backend`** *(optional, string)* — fully-qualified Kotlin
  class the registry hands to the dispatcher. Default: `<T>BackendImpl`
  / `<T>FactoryImpl` in `android_package`, else in the app's codegen
  package.
- **`android_backend_arg`** *(optional, default `"context"`)* — what
  that class's constructor receives: `"context"`, `"activity"`
  (`androidx.activity.ComponentActivity`) or a fully-qualified
  `Activity` subclass (`"androidx.fragment.app.FragmentActivity"`).
- **`ios_backend`** *(optional, string)* — Swift type the registry
  constructs with no arguments. Default: `<T>BackendImpl` /
  `<T>FactoryImpl`.
- **`android_permissions`** *(optional, array)* — Android permissions
  the plugin needs. Each entry is a string or an inline table with an
  upper API bound:
  ```toml
  android_permissions = [
      "android.permission.CAMERA",
      { name = "android.permission.WRITE_EXTERNAL_STORAGE", max_sdk_version = 28 },
  ]
  ```
  Names outside `android.permission.*`, `com.android.vending.*` and
  `com.google.android.*` get a build warning. The app writes the
  permissions of the plugins it wires into
  `android/.istmo/istmo-permissions.xml`, which the Gradle plugin merges
  into every variant's manifest.
- **`[[plugin.gradle]]`** — Gradle deps scoped to this plugin, same keys
  as [`[[gradle]]`](#gradle). Merged with any top-level `[[gradle]]`.
- **`[[plugin.swift_package]]`** — SwiftPM deps scoped to this plugin,
  same keys as [`[[swift_package]]`](#swift_package).
- **`[plugin.android_service]`** *(optional, table)* — declares a
  Kotlin `LifecycleService` shim the app-side build generates. Fields:
  `class_name` (required), `foreground_service_type`, `exported`
  (default `false`), `permission`, `process`. See
  [Services and workers](/istmo/writing-plugins/services-workers/) for the
  full flow.
- **`[plugin.ios_background]`** *(optional, table)* — declares an iOS
  `BGTaskScheduler` shim (or continuous background mode). Fields:
  `class_name` (required), `kind` (required: `refresh` | `processing` |
  `continuous`), `task_identifier`, plus kind-specific keys:
  `interval_minutes` (required) for `refresh`; `requires_power` /
  `requires_network` (default `false`) for `processing`;
  `continuous_mode` (required: `audio` | `location` | `voip` |
  `external_accessory` | `bluetooth_central` | `bluetooth_peripheral`)
  for `continuous`.
- **`[plugin.info_plist]`** *(optional, table)* — top-level `Info.plist`
  entries the plugin needs on iOS (usage descriptions, capability
  flags). Keys are `Info.plist` keys; values are strings or booleans:
  ```toml
  [plugin.info_plist]
  NSFaceIDUsageDescription = "Authenticate to unlock protected content."
  ```
  The app merges the entries of the plugins it wires (first declaration
  wins on conflicting values, with a build warning), applies its own
  [`[app.info_plist]`](#appinfo_plist) overrides and writes them into
  the `Info.plist` managed block and the `Info.plist.background.xml`
  sidecar.

Permissions, `Info.plist` entries, service shims and background shims
only reach the app for the plugins it wires through `istmo::runtime!`
(plus plugins without a `client_type`, which cannot be listed there).

## `[[plugin]]` — array form

Use when a single crate hosts multiple plugin traits. Each entry takes
the same keys as `[plugin]`; two entries may not share an `id`:

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

# Applies to every [[plugin]] in this crate.
[[gradle]]
group    = "androidx.work"
artifact = "work-runtime-ktx"
version  = "2.9.0"
```

## `[[gradle]]`

```toml
[[gradle]]
scope    = "implementation"    # implementation | api | runtimeOnly | compileOnly (default: implementation)
group    = "com.example"       # required
artifact = "widget"            # required
version  = "1.2.3"             # required
```

The app merges the `[[gradle]]` entries of every plugin with its own.
When two crates ask for the same artifact in the same scope at different
versions, the highest version wins and the build prints a warning. The
`io.github.sergioribera.istmo` Gradle plugin adds the result to the app
module.

## `[[swift_package]]`

```toml
[[swift_package]]
url          = "https://github.com/apple/swift-nio"   # required
product      = "NIO"                                  # required
from_version = "2.62.0"                               # required
```

Only `from_version` ("up to next major") is supported. Entries are
merged the same way as Gradle ones (highest `from_version` wins) and
are readable from a build script through
`istmo_build::collect_dep_native_deps().swift_entries()`. The generated
`istmo-plugins.yml` does not declare packages, so add each one to your
Xcode project (the `packages:` section of `project.yml`, or File → Add
Package Dependencies).

## `[[assets]]`

Files bundled with the app, readable at run time through
`istmo::assets`:

```toml
[[assets]]
path = "assets/logo.png"       # a single file

[[assets]]
path = "assets/fonts"          # a directory, walked recursively

[[assets]]
path = "assets/i18n/*.json"    # a glob
```

- **`path`** *(required, non-empty string)* — relative to the crate
  root, and must stay inside it. A path that does not exist, or a glob
  that matches nothing, fails the build.

The lookup key drops a leading `assets/`: the entry
`assets/logo.png` is read with `istmo::assets::read("logo.png")`.
An app's own assets sit at the root of the bundle; a plugin's sit under
`plugin-<id>/` and are read with
`istmo::assets::plugin("<id>").read("…")`.

Where they land:

- **Android** — `android/app/src/main/assets/` (plugins under
  `plugin-<id>/`), packaged by Gradle's asset merger.
- **iOS** — `ios/.istmo/Assets/`, shipped as one folder reference so the
  files land at `<bundle>/Assets/…` (see
  [Native projects](/istmo/build-scripts/native-projects/#ios)).
- **Desktop** — staged under the build's `OUT_DIR` and baked into the
  binary as `ISTMO_APP_ASSETS_DIR`, unless `[app] assets_dir` names
  another directory. The `ISTMO_ASSETS_DIR` environment variable
  overrides both at run time.

Assets that are no longer declared are removed from these trees on the
next build.

---

## `[app]`

App identity and codegen. Every key is optional; defaults in comments:

```toml
[app]
# Identity — applied by the `io.github.sergioribera.istmo` Gradle plugin and the
# generated `ios/.istmo/Istmo.xcconfig` (see Native projects).
id      = "com.myapp"            # applicationId + PRODUCT_BUNDLE_IDENTIFIER (default: left to the native projects)
name    = "My App"               # launcher label / CFBundleDisplayName (default: Cargo package name)
version = "1.2.0"                # versionName / MARKETING_VERSION (default: Cargo package.version)
build   = 12                     # versionCode / CURRENT_PROJECT_VERSION, 1..=2100000000 (default: 1)

# Codegen
android            = true             # default: true (skipped if no ./android)
android_package    = "com.myapp.gen"  # default: "<gradle namespace>.gen"
ios                = true             # default: true (skipped if no ./ios)
ios_app_dir        = "MyApp"          # default: the single ./ios/* app subdir
ios_plugins_subdir = "Plugins"        # default: "Plugins"
auto_register      = true             # default: true (register plugins in IstmoPluginRegistry)
rust_entry         = true             # default: detected (`#[istmo::mobile_app]` in src/); emits IstmoApp.run()

# Platform extras
android_permissions = ["android.permission.INTERNET"]  # same format as [plugin] android_permissions
assets_dir          = "assets"        # desktop asset root (default: the staged [[assets]] dir)
single_instance     = true            # desktop; default: true when any [[app.document_type]] is declared

# Launcher icon (see Native projects). Every key is optional.
[app.icon]
image   = "assets/icon.png"      # iOS app icon + legacy Android mipmaps (default: none)
desktop = "assets/tile.png"      # window icon + Windows .exe icon (default: image)

[app.icon.android]               # adaptive icon, API 26+ (default: none)
foreground = "assets/fg.xml"     # required: vector drawable .xml or .png
background = "#EFE6D4"           # required: #RRGGBB / #AARRGGBB, .xml or .png
monochrome = "assets/mono.xml"   # themed icon, Android 13+ (default: none)
```

- `id` must be a reverse-DNS identifier (`com.example.app`: two or more
  dot-separated segments of ASCII letters, digits and `_`, each starting
  with a letter).
- `android_package` only holds the contracts whose plugin declares no
  `[plugin] android_package` (the app's own plugins and the built-in
  contracts). Official plugins generate into `dev.istmo.plugins.<name>`
  next to their Kotlin sources, so hand-written app code that names one
  of their generated types (a dispatcher registered by hand, a codec)
  imports it from there, e.g.
  `import dev.istmo.plugins.share.ShareDispatcher`.
- `android_permissions` adds permissions the app's own code needs on top
  of the wired plugins' ones.
- `assets_dir` is resolved against the crate root when relative.
- `[min_versions] android` / `ios` are the app's `minSdk` and deployment
  target.

How each value reaches Gradle and Xcode is described in
[Native projects](/istmo/build-scripts/native-projects/).

### `[[app.plugin]]`

Per-plugin overrides (a single `[app.plugin]` table also works):

```toml
[[app.plugin]]
id            = "istmo.echo"
role          = "client"                # host | client (default: host)
platforms     = ["android"]             # subset of ["android", "ios"] (default: every active one)
auto_register = false                   # skip this plugin in the generated registry

[[app.plugin]]
type      = "Notifier"                  # match by generated type name instead of id
role      = "client"
```

Every entry needs `id` or `type`; when both are set, `id` is used. A
contract is matched by its plugin id first, then by its type name.

### `[app.info_plist]`

Overrides (or adds) `Info.plist` entries on top of the ones declared by
plugins through `[plugin.info_plist]` — typically to localise a usage
description:

```toml
[app.info_plist]
NSFaceIDUsageDescription = "Use Face ID to open your vault."
```

Entries land between the managed markers of the app's `Info.plist`,
shared with `[plugin.ios_background]`:

```xml
<dict>
    <!-- istmo:background:start -->
    <!-- istmo:background:end -->
</dict>
```

Without the markers, copy the entries from the generated
`Info.plist.background.xml` sidecar by hand.

### `[[app.document_type]]`

File types the app opens. One declaration feeds every platform:

```toml
[app]
single_instance = true          # desktop; default: true when any document_type is declared

[[app.document_type]]
name       = "PDF document"     # required: name the system shows
id         = "com.adobe.pdf"    # UTI (Apple) / ProgID base (Windows); required when exported
extensions = ["pdf"]            # without the dot
mime_types = ["application/pdf"]
role       = "viewer"           # viewer | editor (default: viewer)
rank       = "alternate"        # owner | default | alternate (default: alternate)

[[app.document_type]]
name        = "Notes document"
id          = "com.example.notes.document"
extensions  = ["note"]
mime_types  = ["application/x-note"]
role        = "editor"
rank        = "owner"           # only on exported types
exported    = true              # the app defines this type (default: false)
conforms_to = ["public.data"]   # Apple parent types (default: ["public.data"])
```

Every entry needs `extensions` or `mime_types`; an exported entry needs
`id` and `extensions`. Extensions are written without the leading dot,
MIME types as `type/subtype`.

- **Android** — `VIEW` and `SEND` intent filters by MIME type, plus a
  path-pattern `VIEW` filter for the extensions of exported types. They
  belong to one activity, so they land between markers you place inside
  the `<activity>` that receives documents:

  ```xml
  <activity android:name="dev.istmo.runtime.IstmoGameActivity" ...>
      <!-- istmo:documents:start -->
      <!-- istmo:documents:end -->
  </activity>
  ```

  Without the markers, copy them from `app/src/main/AndroidManifest.documents.xml`.
- **iOS** — `CFBundleDocumentTypes`, `UTExportedTypeDeclarations`,
  `UTImportedTypeDeclarations` (system types such as `com.adobe.pdf`
  are not re-imported) and, when a type is an `editor`,
  `LSSupportsOpeningDocumentsInPlace`, written between the
  `istmo:background` markers of `Info.plist`.
- **Desktop** — packaging tools read the same table (for example
  nix-bundle-app's `fileAssociations`). At run time,
  `istmo::plugins::DocumentsPublisher` publishes the documents of the
  command line and, with `single_instance`, forwards the documents of a
  later launch to the running instance (Unix socket guarded by an
  `flock` on Linux and macOS, named pipe on Windows). The build emits
  `ISTMO_SINGLE_INSTANCE`; `istmo::single_instance!()` turns it into the
  instance key.

Opened documents arrive on the `istmo.documents` plugin
(`Documents::stream()`); Android and iOS shells publish to the same
channel from their intent / URL handlers.

## `[[remote_override]]`

Override a plugin's `default_deployment` from the app:

```toml
[[remote_override]]
plugin     = "acme.large_model"       # required: plugin id
deployment = "remote"                 # required: local | remote
```

Auto-wiring then declares the plugin's client remote (or local) in
`istmo::runtime!`. See [Remote process](/istmo/advanced/remote-process/)
for the bridge shape.

---

## `[min_versions]`

Oldest OS release the crate supports. Every key is optional.

| Key | Type | Meaning |
|---|---|---|
| `android` | integer | Android API level (`minSdk`). |
| `ios` | string | iOS deployment target, e.g. `"15.0"`. |
| `macos` | string | macOS deployment target, e.g. `"11.0"`. |
| `windows` | string | Windows build, e.g. `"10.0.17763"`. |

Versions are `major[.minor[.patch]]`; a bare integer (`ios = 15`) is
accepted too.

```toml
[min_versions]
android = 24
ios     = "15.0"
macos   = "11.0"
windows = "10.0.17763"
```

On a **plugin** crate these are requirements. On an **app** crate they
are the floor the app ships to. `istmo_build::emit()` compares every
plugin dependency against the app floor of the target being compiled
and fails the build, listing every offending plugin, when one needs a
newer OS. The floor is taken from the app's own `[min_versions]` when
present; otherwise it is detected:

| Target | Detected from |
|---|---|
| Android | `minSdk` in `android/app/build.gradle(.kts)` |
| iOS | `$IPHONEOS_DEPLOYMENT_TARGET` |
| macOS | `$MACOSX_DEPLOYMENT_TARGET` |
| Windows | only the app's `[min_versions]` |

With no floor to compare against, the check is skipped. Other targets
(Linux, wasm, …) are never checked.

The platform tools repeat the check on their own: the
`io.github.sergioribera.istmo` Gradle plugin compares `android` against
each variant's resolved `minSdk`, and the generated `istmo-plugins.yml`
adds an Xcode pre-build script that compares the strictest plugin `ios`
against `IPHONEOS_DEPLOYMENT_TARGET`.

## Native manifest fragments

Plugins that need entries in the host app's platform manifests beyond
permissions and `Info.plist` keys ship them as files next to their
native sources; nothing goes in `istmo.toml`.

| File | Merged into | By |
|---|---|---|
| `native/android/AndroidManifest.xml` | every variant's manifest | `io.github.sergioribera.istmo` Gradle plugin |
| `native/android/res/` | app resources | `io.github.sergioribera.istmo` Gradle plugin |
| `native/ios/Info.plist.fragment` | app `Info.plist` | `istmo-build` |
| `native/ios/App.entitlements.fragment` | app `.entitlements` | `istmo-build` |

The Android fragment is a regular library-style manifest
(`<manifest>` with `<application>` children, `<queries>`,
`<uses-permission>`); `${applicationId}` placeholders work.

The Apple fragments are property lists whose top-level value is a
`<dict>`. `istmo-build` merges them (dicts recursively, arrays
concatenated, first plugin wins on scalar conflicts, the app always
wins over plugins) and writes the result between markers placed inside
the app's `<dict>`:

```xml
<!-- istmo:plugins:start -->
<!-- istmo:plugins:end -->
```

Without markers, the merged keys are written to a sidecar
(`Info.plist.plugins.xml`, `<App>.entitlements.plugins.xml`) to copy by
hand.

### `[[windows_manifest_fragment]]`

Windows application-manifest entries do live in `istmo.toml` (plugin or
app). Both keys are required; each `xml` is a direct child of
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

When the app builds for Windows, `istmo-build` keeps one fragment per
`name` (first declaration wins), wraps them in an `<assembly>` and
embeds the result: `/MANIFEST:EMBED /MANIFESTINPUT:` on MSVC, an
`RT_MANIFEST` resource next to the `.exe` icon on GNU.

## Full example (plugin crate)

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

## Full example (app crate)

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

## Errors

Every error names the key and stops the build in `build.rs`, before any
code is generated:

- **Unknown key** → ``istmo.toml unknown key `widget` `` (nested keys
  are spelled out: `` `plugin.foo` ``, `` `app.andorid_package` ``).
  The `[app]` section is validated as strictly as the rest.
- **Wrong type** → ``istmo.toml key `plugin.id` expected string``.
- **Missing required key** → ``istmo.toml missing required key `plugin.id` ``.
- **Invalid value** → ``istmo.toml key `app.id` = `demo` (expected a
  reverse-DNS id such as `com.example.app`)``. The same shape reports a
  bad plugin id, `client_type`, Kotlin package, build number, duplicate
  `[[plugin]]` id, icon layer or document type.
- **Unknown enum value** → for `deployment` / `default_deployment`,
  Gradle `scope`, `ios_background.kind` and `continuous_mode`, the
  message lists the accepted values.
- **Invalid version** → ``istmo.toml key `min_versions.ios` = `15.x`
  (expected an API level integer for `android`, or a
  `major[.minor[.patch]]` string)``.
- **TOML syntax** → `parsing istmo.toml: …` with the parser's location.
