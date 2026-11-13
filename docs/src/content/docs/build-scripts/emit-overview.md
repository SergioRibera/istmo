---
title: emit() overview
description: One entry point drives both plugin metadata handover and app-side Kotlin/Swift codegen.
sidebar:
  order: 1
---

`istmo_build::emit()` is the only function you should ever need in a
`build.rs`. It looks at the crate it runs in and does whatever that
crate needs.

## The one-liner

```rust
// build.rs for a plugin — or an app — or both
fn main() {
    istmo_build::emit();
}
```

`istmo-build` goes in `[build-dependencies]`. In one run, `emit()`:

1. Reads and validates `istmo.toml` (a missing file is an empty
   configuration).
2. If the file declares `[plugin]` / `[[plugin]]` entries, hands the
   plugin metadata over to dependent crates.
3. Bakes the auto-wired plugin list into `ISTMO_AUTO_PLUGINS` /
   `ISTMO_AUTO_REMOTE` for `istmo::runtime!`.
4. Checks every plugin's `[min_versions]` against the app's floor for
   the target being compiled.
5. Runs the app-side pass when the crate is an app: Kotlin/Swift codegen
   and native project sync when `android/` or `ios/` sit next to
   `Cargo.toml`, desktop extras when the crate builds a binary.

## Decision flow

```
emit()
 │
 ├─ istmo.toml has [plugin]? ── yes ─► cargo:ISTMO_MANIFEST, cargo:PLUGIN_IDS
 │                                     cargo:ISTMO_CONTRACTS   (entries with client_type)
 │                                     cargo:ISTMO_NATIVE_DEPS (when [[gradle]] / [[swift_package]])
 │                                     cargo:ISTMO_ASSETS      (when [[assets]])
 │                                     cargo:ISTMO_NATIVE_<OS> (per native/<os>/ dir)
 │
 ├─ always ─► ISTMO_AUTO_PLUGINS / ISTMO_AUTO_REMOTE (rustc-env)
 │            [min_versions] check for the target OS
 │
 ├─ android/, ios/ or a binary target? ── no ─► done (plain plugin crate)
 │
 ├─ yes ─► ISTMO_APP_BUNDLE_ID, ISTMO_APP_ASSETS_DIR, ISTMO_SINGLE_INSTANCE
 │         desktop window icon + Windows resources (desktop targets)
 │         stage [[assets]] for every active platform
 │
 └─ android/ or ios/ active? ── yes ─► walk collect_dep_contracts()
                                       write <T>Dispatcher / Types / CodecsImpl (.kt / .swift)
                                       write IstmoPluginRegistry (+ IstmoMain.swift)
                                       sync ios/.istmo/, and android/.istmo/ on Android targets
                                       permissions, services, plist entries
                                       doctor warnings
```

Both halves can run in the same invocation — an app crate that also
declares its own plugin is valid.

## When you need more control

Use `emit_with(AppOpts)` to pass:

- `extra_contracts: Vec<Contract>` — for contracts no dependency hands
  over: ones built by a build-time schema crate or inline in `build.rs`.
  A plugin crate with several `[[plugin]]` entries already hands over
  all of them. An extra contract with the plugin id of a handed-over
  one replaces it (with a warning).
- `extra_manifests` / `extra_native_ios_dirs` — plugin manifests and
  `native/ios/` directories to treat as if a dependency had handed them
  over.
- `per_plugin: HashMap<String, AppPluginOpts>` — override the role
  (`host` vs. `client`), platforms or `auto_register` per plugin id or
  type name, on top of `[[app.plugin]]`.
- `seed_backends: true` — generate one-time `<T>BackendImpl.kt/swift`
  scaffolds with `TODO()` / `fatalError` bodies (written only when
  absent, never overwritten). Skipped for plugins that ship their own
  backend.
- `auto_register: Some(false)` — skip the plugin registries' entries.
- `lib_name` — the native library name used by generated service shims
  (default: the crate's lib name).
- `root`, `android`, `android_root`, `android_package`, `ios`,
  `ios_root`, `ios_app_dir`, `ios_plugins_subdir` — override
  auto-detection and the matching `[app]` keys.

Example, from `examples/rust-mobile-demo/build.rs`:

```rust
// build.rs (at the app crate root)
use istmo_build::AppOpts;
use istmo_plugins_schema as plugin_contract;

fn main() {
    istmo_build::emit_with(AppOpts {
        extra_contracts: vec![
            plugin_contract::permissions(),
            plugin_contract::notifications(),
            plugin_contract::admob(),
        ],
        ..AppOpts::default()
    });
}
```

`emit_from(manifest_path, source_path)` is the plugin-side half alone,
for a plugin crate whose `istmo.toml` or trait source is not at the
default location; `emit_manifest_metadata_with_contracts(path,
&contracts)` hands over hand-built contracts instead of extracted ones.

## Auto-detection details

- **Android package**: parsed from `android/app/build.gradle{,.kts}`
  `namespace = "…"`. Fallback to `AndroidManifest.xml package="…"`.
  Default target package is `<namespace>.gen`.
- **iOS app dir**: scanned from `ios/*/`. If exactly one non-hidden
  subdirectory exists (excluding `*.xcodeproj` and `*.xcworkspace`),
  that is the app dir. With ≥2 candidates, the one paired with a
  `.xcodeproj` sibling wins.

Override either via `[app] android_package = "…"` or
`[app] ios_app_dir = "…"` in the app's `istmo.toml`. When neither
resolves, the build warns and skips that platform's sources.

A plugin that ships its own Kotlin backend declares
`[plugin] android_package` in its `istmo.toml`; its dispatcher, types
and codecs are generated into that package instead of the app's, so
the plugin's `native/android/` sources compile under any app package.
Only contracts without one (the app's own plugins, built-in contracts)
follow `[app] android_package`.

## Generated files

For each contract and active platform (role `host` unless overridden):

| Platform | Location | Files |
| --- | --- | --- |
| Android | `android/app/src/main/java/<package>/` | `<T>Dispatcher.kt`, `<T>Types.kt`, `<T>CodecsImpl.kt` (role `client`: `<T>Codecs.kt` + `<T>Client.kt` instead of the dispatcher) |
| iOS | `ios/<AppDir>/<PluginsSubdir>/<T>/Generated/` | `<T>Dispatcher.swift`, `<T>Types.swift`, `<T>CodecsImpl.swift` (role `client`: `<T>Client.swift` instead of the dispatcher) |

Kotlin files import only the `dev.istmo.runtime` types they use, so they
compile in any package.

## Skip a platform

If you only ship Android or only ship iOS, tell `emit()` to skip the
other platform even if the directory exists:

```toml
[app]
ios = false          # never generate Swift files
```

## Testing without side effects

Generated files are written only when their contents change, and every
input the pass reads is tracked with `cargo:rerun-if-changed`, so a
rebuild with unchanged inputs touches nothing. If you need to inspect
what would be emitted, print manifests in `build.rs`:

```rust
if let Ok(m) = istmo_build::Manifest::from_path("istmo.toml") {
    println!("cargo::warning=parsed manifest: {m:#?}");
}
```

## Auto-registration

Alongside per-plugin dispatcher, types, and codec files, `emit()`
generates a single `IstmoPluginRegistry.kt` / `.swift` that wires every
eligible plugin's dispatcher in one call. See
[Auto-registration](/istmo/build-scripts/auto-register/) for the full story.

## Next

- Full manifest schema: [`istmo.toml` reference](/istmo/build-scripts/istmo-toml-reference/).
- Understanding the DEP_* mechanism: [Concepts → Contracts](/istmo/concepts/contracts/).
- Auto-wiring `runtime!`: [Advanced → Auto-wiring](/istmo/advanced/auto-wiring/).
- Auto-registering dispatchers: [Auto-registration](/istmo/build-scripts/auto-register/).
