---
title: istmo.toml reference
description: Every key, every default, every valid value.
sidebar:
  order: 2
---

Every plugin crate and every app crate can have an `istmo.toml`. Every
section is optional; an empty file is legal.

## Top-level tables

| Key                  | Type              | Where       | Purpose                                       |
| -------------------- | ----------------- | ----------- | --------------------------------------------- |
| `[plugin]`           | table / array     | plugin      | Declare one or more plugin traits             |
| `[[gradle]]`         | array of tables   | plugin, app | Extra Gradle dependencies (shared across all plugins in the crate) |
| `[[swift_package]]`  | array of tables   | plugin, app | Extra SwiftPM packages                        |
| `[[remote_override]]`| array of tables   | app         | Force a specific plugin to the `:remote` process |
| `[app]`              | table             | app         | App-side Kotlin/Swift codegen configuration   |

Unknown top-level keys are rejected — expect a build-time error if you
typo a section.

---

## `[plugin]` — single form

Use when a crate hosts exactly one plugin trait:

```toml
[plugin]
id                  = "acme.telemetry"
client_type         = "::acme_telemetry::TelemetryClient"
default_deployment  = "local"          # or "remote"

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

- **`id`** *(required, string)* — plugin identifier. Convention:
  `<org>.<name>` (`istmo.data_store`, `acme.telemetry`).
- **`client_type`** *(optional, string)* — fully-qualified path to the
  Rust client type (`::crate::FooClient`). If set, `emit()` extracts
  the corresponding trait's contract from `src/lib.rs`. Omit for
  plugins that carry only shared types.
- **`default_deployment`** *(optional, `"local"` \| `"remote"`,
  default `"local"`)* — where the plugin's backend should run by
  default. Apps can override via `[[remote_override]]`.
- **`auto_register`** *(optional, bool, default `true`)* — whether
  `emit_app` should include this plugin in the generated
  `IstmoPluginRegistry.registerAll(...)`. Set to `false` when the
  plugin's `BackendImpl` / `FactoryImpl` needs a non-standard
  constructor and the app author must register the dispatcher
  manually. See [Auto-registration](/istmo/build-scripts/auto-register/).
- **`[[plugin.gradle]]`** — Gradle deps scoped to this plugin. Merged
  with any top-level `[[gradle]]`.
- **`[[plugin.swift_package]]`** — SwiftPM deps scoped to this plugin.
- **`[plugin.android_service]`** *(optional, table)* — declares a
  Kotlin `LifecycleService` shim the app-side `emit_app` should
  generate. Fields: `class_name` (required), `foreground_service_type`,
  `exported` (default `false`), `permission`, `process`. See
  [Services and workers](/istmo/writing-plugins/services-workers/) for the
  full auto-wire flow.
- **`[plugin.ios_background]`** *(optional, table)* — declares an iOS
  `BGTaskScheduler` shim (or continuous background mode). Fields:
  `class_name` (required), `task_identifier`, `kind` (`refresh` |
  `processing` | `continuous`), plus kind-specific keys:
  `interval_minutes` for `refresh`; `requires_power` /
  `requires_network` for `processing`; `continuous_mode` (`audio` |
  `location` | `voip` | `external_accessory` | `bluetooth_central` |
  `bluetooth_peripheral`) for `continuous`.
- **`[plugin.info_plist]`** *(optional, table)* — top-level `Info.plist`
  entries the plugin needs on iOS (usage descriptions, capability
  flags). Keys are `Info.plist` keys; values are strings or booleans:
  ```toml
  [plugin.info_plist]
  NSFaceIDUsageDescription = "Authenticate to unlock protected content."
  ```
  `emit_app` merges the entries of every plugin (first declaration
  wins on conflicting values, with a build warning), applies the app's
  [`[app.info_plist]`](#appinfo_plist) overrides and writes them into
  the `Info.plist` managed block and the `Info.plist.background.xml`
  sidecar.

## `[[plugin]]` — array form

Use when a single crate hosts multiple plugin traits:

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

# Applies to every [[plugin]] in this crate.
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

Only `.from_version` is supported today; branch and revision
constraints will land alongside the first plugin that needs them.

---

## `[app]` — app-side codegen

Every field is optional. Defaults in comments:

```toml
[app]
android         = true                             # default: true (skipped if no ./android)
android_package = "com.myapp.gen"                  # default: "<gradle namespace>.gen"
ios             = true                             # default: true (skipped if no ./ios)
ios_app_dir     = "MyApp"                          # default: single ./ios/* subdir
ios_plugins_subdir = "Plugins"                     # default: "Plugins"
auto_register   = true                             # default: true (emit IstmoPluginRegistry)
```

### `[[app.plugin]]`

Per-plugin overrides:

```toml
[[app.plugin]]
id            = "istmo.echo"
role          = "client"                # host | client (default: host)
platforms     = ["android"]             # subset of ["android", "ios"]
auto_register = false                   # skip this plugin in the generated registry

[[app.plugin]]
type      = "Notifier"                  # match by generated type name instead of id
role      = "client"
```

Match rule: `id` takes precedence, then `type`. Either matches by
string equality.

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

## `[[remote_override]]`

Force a plugin's client to route through the `:remote` process on
Android:

```toml
[[remote_override]]
plugin     = "istmo.push_notifications"
deployment = "remote"                   # local | remote
```

See [Remote process](/istmo/advanced/remote-process/) for the bridge shape.

---

## Full example (plugin crate)

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

## Full example (app crate)

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

## Errors

- **Unknown top-level key** → `istmo.toml unknown key "widget"`.
  Add it to the schema or remove the section.
- **Unknown plugin key** → `istmo.toml unknown key "plugin.foo"`.
- **Wrong type** → `istmo.toml key "plugin.id" expected string`.
- **Missing required key** → `istmo.toml missing required key "plugin.id"`.

All errors are reported at `build.rs` time — the build stops before
any code is generated.
