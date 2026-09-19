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

- Reads `istmo.toml` if present.
- Emits plugin metadata (contract + native deps) if `[plugin]` is set.
- Emits app-side wiring env vars if the crate depends on plugins.
- Generates Kotlin/Swift dispatchers if `android/` or `ios/` sit next
  to `Cargo.toml`.

## Decision flow

```
                     ┌─ emit() ────────────────────────────────┐
                     │                                          │
     istmo.toml has  │                                          │
     [plugin]?       ├── yes ─► emit contract via DEP_*         │
                     │           emit native deps via DEP_*     │
                     │           emit manifest via DEP_*         │
                     │                                          │
     android/ or     │                                          │
     ios/ dir next   ├── yes ─► emit_wiring_env (auto-wiring)   │
     to Cargo.toml?  │           run emit_app_with(AppOpts)     │
                     │           - walk collect_dep_contracts() │
                     │           - auto-detect namespaces/dirs  │
                     │           - write generated .kt/.swift   │
                     │                                          │
                     └─ done ──────────────────────────────────┘
```

Both branches can run in the same `emit()` invocation — an app crate
that also declares its own plugin is valid.

## When you need more control

Use `emit_with(AppOpts)` to pass:

- `extra_contracts: Vec<Contract>` — for schema crates that carry
  multiple contracts, or testbeds with inline contracts that never
  ship a plugin crate.
- `per_plugin: HashMap<String, AppPluginOpts>` — override the role
  (`host` vs. `client`) or platforms per plugin without touching
  `istmo.toml`.
- `seed_backends: true` — generate one-time `<T>BackendImpl.kt/swift`
  scaffolds with `TODO()` bodies (`write_if_absent`, never overwrites).
- `root`, `android_root`, `ios_root`, `android_package`, `ios_app_dir`
  — override auto-detection.

Example:

```rust
// build.rs (at the app crate root)
use istmo_build::{AppOpts, emit_with};
use my_schema_crate::plugin_contract;

fn main() {
    emit_with(AppOpts {
        extra_contracts: vec![
            plugin_contract::permissions(),
            plugin_contract::notifications(),
        ],
        seed_backends: true,
        ..AppOpts::default()
    });
}
```

## Auto-detection details

- **Android package**: parsed from `android/app/build.gradle{,.kts}`
  `namespace = "…"`. Fallback to `AndroidManifest.xml package="…"`.
  Default target package is `<namespace>.gen`.
- **iOS app dir**: scanned from `ios/*/`. If exactly one non-hidden
  subdirectory exists (excluding `*.xcodeproj` and `*.xcworkspace`),
  that is the app dir. With ≥2 candidates, the one paired with a
  `.xcodeproj` sibling wins.

Override either via `[app] android_package = "…"` or
`[app] ios_app_dir = "…"` in the app's `istmo.toml`.

## Runtime imports

Kotlin dispatchers depend on classes from the `dev.istmo.runtime`
package (`IstmoRuntime`, `Bincode`, `PluginException`, etc.). When
the target package is not `dev.istmo.runtime`, `emit()` prepends an
import block automatically — you never write those imports by hand.

## Skip a platform

If you only ship Android or only ship iOS, tell `emit()` to skip the
other platform even if the directory exists:

```toml
[app]
ios = false          # never generate Swift files
```

The auto-detect + generator loop short-circuits for the disabled
platform.

## Testing without side effects

`emit()` in a plugin crate is idempotent — repeated runs of `cargo
build` regenerate the same bytes. `write_if_changed` skips the
filesystem when contents match. If you need to inspect what would be
emitted, print manifests in `build.rs`:

```rust
if let Ok(m) = istmo_build::Manifest::from_path("istmo.toml") {
    println!("cargo::warning=parsed manifest: {m:#?}");
}
```

## Auto-registration

Alongside per-plugin dispatcher, types, and codec files, `emit()`
generates a single `IstmoPluginRegistry.kt` / `.swift` that wires every
eligible plugin's dispatcher in one call. See
[Auto-registration](/build-scripts/auto-register/) for the full story.

## Next

- Full manifest schema: [`istmo.toml` reference](/build-scripts/istmo-toml-reference/).
- Understanding the DEP_* mechanism: [Concepts → Contracts](/concepts/contracts/).
- Auto-wiring `runtime!`: [Advanced → Auto-wiring](/advanced/auto-wiring/).
- Auto-registering dispatchers: [Auto-registration](/build-scripts/auto-register/).
