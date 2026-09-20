---
title: The manifest
description: What istmo.toml is, why it exists, and how each section flows through the build.
sidebar:
  order: 4
---

`istmo.toml` sits next to `Cargo.toml` and declares everything the
framework needs to know that is not derivable from Rust code:

- The plugin identifier (a string, not the crate name).
- The Rust client type path.
- Deployment defaults (local vs. `:remote` process).
- Native dependencies — Gradle coordinates and SwiftPM packages.
- App-side codegen overrides (`[app]` section).

Both plugin crates and app crates can carry an `istmo.toml`. The file is
optional for pure-Rust plugin crates that ship nothing native, but the
convention is: **if your crate touches Istmo, it has an `istmo.toml`.**

## The three flavours

### Single-plugin plugin crate

Most common shape:

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

`istmo_build::emit()` reads this, verifies the trait exists, and emits
a bincoded contract to `DEP_<crate>_ISTMO_CONTRACT`, plus the native
deps to `DEP_<crate>_NATIVE_DEPS`.

### Multi-plugin plugin crate

Rare but supported — one crate publishing multiple plugin traits (e.g.
`istmo-plugins` bundles Permissions, Notifications, AdMob):

```toml
[[plugin]]
id          = "istmo.permissions"
client_type = "::istmo_plugins::PermissionsClient"

[[plugin]]
id          = "istmo.notifications"
client_type = "::istmo_plugins::NotificationsClient"

# Shared native deps across every plugin in the crate.
[[gradle]]
group    = "androidx.core"
artifact = "core-ktx"
version  = "1.13.0"
```

`istmo_build::emit()` iterates every `[[plugin]]` entry and extracts one
contract per trait.

### App-side manifest

An app crate uses `istmo.toml` only for **overrides**:

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

Everything under `[app]` and `[[app.plugin]]` shapes the Kotlin/Swift
codegen — see [`emit_app` overview](/istmo/build-scripts/emit-overview/) and
the [full reference](/istmo/build-scripts/istmo-toml-reference/).

`[[remote_override]]` flips a plugin's default deployment so its client
class routes through the `:remote` bridge instead of the local runtime.

## What lives outside the manifest

- **The trait signatures.** They come from your Rust source; `syn`
  parses them into a `Contract`.
- **Wire versioning.** The protocol carries its own version byte
  (`PROTOCOL_VERSION` in `istmo-core`).
- **Runtime wiring.** `istmo::runtime!` macro invocation, not TOML.

Keeping the boundary explicit means you never chase a change through
two files.

## Next

- Full field-by-field reference: [`istmo.toml` reference](/istmo/build-scripts/istmo-toml-reference/).
- Understand how the app-side `[app]` section drives codegen: [Build
  scripts overview](/istmo/build-scripts/emit-overview/).
