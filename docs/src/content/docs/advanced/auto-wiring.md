---
title: Auto-wiring
description: How the runtime! macro picks up plugins from your Cargo dependencies without a manual list.
sidebar:
  order: 3
---

`istmo::runtime!` supports Flutter-style **transparent auto-wiring**.
You add a plugin to your `Cargo.toml`, and it appears on the runtime —
no manual `plugins:` list, no `use` line.

The trick uses Cargo's `DEP_*` env var mechanism, `emit_wiring_env` in
`build.rs`, and env-var pickup at macro expansion time. This page
explains the mechanism so you can debug or extend it.

## The mechanism, end-to-end

### 1. Plugin `build.rs`

```rust
fn main() { istmo_build::emit(); }
```

`emit()` reads `istmo.toml`, emits the manifest to
`cargo:ISTMO_MANIFEST=<hex>`, which Cargo re-exports to every dependent
as `DEP_<links>_ISTMO_MANIFEST`.

### 2. App `build.rs`

```rust
fn main() { istmo_build::emit(); }
```

The app-side `emit()` — via `emit_wiring_env` — walks every
`DEP_*_ISTMO_MANIFEST` env var, decodes the manifest, applies any
`[[remote_override]]` from the app's own `istmo.toml`, and writes:

- `cargo::rustc-env=ISTMO_AUTO_PLUGINS=Foo,Bar,Baz`
- `cargo::rustc-env=ISTMO_AUTO_REMOTE=Qux`

Cargo makes these env vars available at compile time (`rustc-env`
scope).

### 3. `runtime!` macro

At expansion, `istmo::runtime!` reads `ISTMO_AUTO_PLUGINS` and
`ISTMO_AUTO_REMOTE` via `std::env::var_os`, parses each entry as a
`syn::Path`, and appends it to whatever the user wrote:

```rust
// Your source:
istmo::runtime!(
    plugins: [MyOwnClient],
);

// After expansion (conceptually):
Runtime::new(RuntimeInit {
    plugins: vec![MyOwnClient::expect(), SignInClient::expect(), DataStoreClient::expect()],
    remotes: vec![],
    services: vec![],
    workers: vec![],
})
```

Duplicates (a plugin listed both manually and auto-wired) are
collapsed via a HashSet — safe to write both.

## What gets auto-wired

Only plugins that:

- Have a `client_type` set in their `istmo.toml`.
- Come from a Cargo dependency that ships an `istmo.toml`.
- Are not opted out (see below).

Plugins that carry only shared types (no `client_type`) are skipped —
they need no runtime registration.

## Deployment target

Each plugin has a `default_deployment` — `local` or `remote`. Auto-wiring
respects it:

- `local` → the client is added to `plugins`.
- `remote` → the client is added to `remote`.

App-side `[[remote_override]]` flips a plugin's placement without
touching the plugin crate.

## Opting out per plugin

There is no built-in "skip this plugin" flag today — every plugin in
your dependency graph auto-wires. If you want to conditionally
register, drop auto-wiring and list every plugin manually:

```rust
// build.rs (at the app crate root) — skips emit_wiring_env entirely
fn main() {
    istmo_build::emit_manifest_metadata_with_contract("istmo.toml", &contract);
    // no emit_wiring_env → ISTMO_AUTO_* never set → runtime! only sees manual list
}
```

Or:

```rust
// src/lib.rs (at the app crate root)
istmo::runtime!(
    plugins: [SignInClient, DataStoreClient],   // only these two — omit others
);
```

## Debugging

Auto-wiring failures are usually one of:

- **Plugin has no `client_type`** — check the plugin's `istmo.toml`.
- **Plugin dependency has no `links = "…"`** — Cargo drops metadata
  emissions when `links` is missing. Every plugin crate must carry
  `links` in `[package]`.
- **`istmo::runtime!` is expanded in a crate that doesn't run
  `emit_wiring_env`** — the env vars are set only for the crate whose
  `build.rs` calls it.

Print the effective wiring during build:

```rust
let resolved = istmo_build::emit_wiring_env(Some(&std::path::Path::new("istmo.toml")));
println!("cargo::warning=local: {:?}", resolved.local_clients);
println!("cargo::warning=remote: {:?}", resolved.remote_clients);
```

## Why env vars and not `inventory`?

`inventory` / `linkme` do link-time collection — every crate registers
into a global table via a linker section. It works, but:

- Debug builds hit LLD/gold quirks around section merging.
- Static-library targets (iOS) sometimes drop sections without a
  reference.
- Env-var-driven `runtime!` is fully deterministic at
  `cargo build` time — no linker involvement.

The trade-off is that auto-wiring only sees plugins from **direct**
Cargo dependencies. Transitive plugins do not auto-wire — you need to
depend on them yourself if you want them registered.

## Next

- Runtime setup shape: [Concepts → Architecture](/istmo/concepts/architecture/).
- Deploying to `:remote`: [Advanced → Remote process](/istmo/advanced/remote-process/).
