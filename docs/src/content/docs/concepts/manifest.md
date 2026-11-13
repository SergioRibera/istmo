---
title: The manifest
description: What istmo.toml is, why it exists, and how each section flows through the build.
sidebar:
  order: 4
---

`istmo.toml` sits next to `Cargo.toml` and declares everything the
framework needs to know that is not derivable from Rust code:

- The plugin identifier and the Rust client type path.
- Deployment defaults (local vs. `:remote` process).
- Native dependencies — Gradle coordinates and SwiftPM packages.
- Platform needs — Android permissions, `Info.plist` entries, minimum
  OS versions, bundled assets.
- App identity and codegen settings (`[app]` section).

Both plugin crates and app crates can carry an `istmo.toml`. The file is
optional — a missing file is an empty configuration — but a plugin crate
needs one for its contract to reach the app: `emit()` only extracts the
contracts of `[plugin]` entries with a `client_type`.

## The three flavours

### Single-plugin plugin crate

Most common shape:

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

`istmo_build::emit()` reads this, extracts the `Telemetry` trait's
contract from `src/lib.rs` and hands it over as
`DEP_<links>_ISTMO_CONTRACTS`, plus the parsed manifest as
`DEP_<links>_ISTMO_MANIFEST` and the native deps as
`DEP_<links>_ISTMO_NATIVE_DEPS`.

### Multi-plugin plugin crate

One crate can publish several plugin traits with the `[[plugin]]`
array form:

```toml
[[plugin]]
id          = "acme.permissions"
client_type = "::acme_platform::PermissionsClient"

[[plugin]]
id          = "acme.notifications"
client_type = "::acme_platform::NotificationsClient"

# Shared native deps across every plugin in the crate.
[[gradle]]
group    = "androidx.core"
artifact = "core-ktx"
version  = "1.13.0"
```

`istmo_build::emit()` iterates every `[[plugin]]` entry, extracts one
contract per trait and hands all of them over in one payload, so the app
needs no `extra_contracts` for them. Two entries with the same `id` fail
the build.

### App-side manifest

An app crate uses `istmo.toml` for its identity, codegen settings and
overrides:

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

Everything under `[app]` and `[[app.plugin]]` shapes the app identity
and the Kotlin/Swift codegen — see [`emit()` overview](/istmo/build-scripts/emit-overview/)
and the [full reference](/istmo/build-scripts/istmo-toml-reference/).

`[[remote_override]]` flips a plugin's default deployment, so
auto-wiring declares it remote and its frames route through the
`:remote` bridge instead of the local transport.

## What lives outside the manifest

- **The trait signatures.** They come from your Rust source; `syn`
  parses them into a `Contract`.
- **Wire versioning.** The protocol carries its own version
  (`PROTOCOL_VERSION` in `istmo-core`).
- **Runtime wiring.** `istmo::runtime!` macro invocation, not TOML.
- **Platform manifest entries.** Providers, receivers, intent filters,
  extra `Info.plist` keys and entitlements ship as files next to the
  native sources (`native/android/AndroidManifest.xml`,
  `native/ios/Info.plist.fragment`, …) and are merged into the app —
  see [native manifest fragments](/istmo/build-scripts/istmo-toml-reference/#native-manifest-fragments).
  Permissions, usage descriptions and minimum OS versions do live in the
  manifest (`android_permissions`, `[plugin.info_plist]`,
  [`[min_versions]`](/istmo/build-scripts/istmo-toml-reference/#min_versions)).

Keeping the boundary explicit means you never chase a change through
two files.

## Next

- Full field-by-field reference: [`istmo.toml` reference](/istmo/build-scripts/istmo-toml-reference/).
- Understand how the app-side `[app]` section drives codegen: [Build
  scripts overview](/istmo/build-scripts/emit-overview/).
