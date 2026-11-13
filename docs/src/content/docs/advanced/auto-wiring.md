---
title: Auto-wiring
description: How the runtime! macro picks up plugins from your Cargo dependencies without a manual list.
sidebar:
  order: 3
---

`istmo::runtime!` supports Flutter-style **transparent auto-wiring**.
You add a plugin to your `Cargo.toml`, and it appears on the runtime —
no manual `plugins:` list, no `use` line.

The trick uses Cargo's `DEP_*` env var mechanism, `istmo_build::emit()`
in `build.rs`, and env-var pickup at macro expansion time. This page
explains the mechanism so you can debug or extend it.

## The mechanism, end-to-end

### 1. Plugin `build.rs`

```rust
fn main() { istmo_build::emit(); }
```

`emit()` reads `istmo.toml` and prints the parsed manifest as
`cargo:ISTMO_MANIFEST=<hex>`. Because the plugin crate sets
`links = "…"` in its `[package]`, Cargo re-exports it to every crate
that depends on it directly as `DEP_<links>_ISTMO_MANIFEST`.

### 2. App `build.rs`

```rust
fn main() { istmo_build::emit(); }
```

The app-side `emit()` walks every `DEP_*_ISTMO_MANIFEST` env var,
decodes the manifest, applies any `[[remote_override]]` from the app's
own `istmo.toml`, and writes the client types it found:

- `cargo::rustc-env=ISTMO_AUTO_PLUGINS=::istmo_google_sign_in::SignInClient,::istmo_data_store::DataStoreClient`
- `cargo::rustc-env=ISTMO_AUTO_REMOTE=::acme_model::LargeModelClient`

Both are always set, empty when nothing applies. `rustc-env` makes them
visible to the macros while the app crate compiles.

### 3. `runtime!` macro

At expansion, `istmo::runtime!` reads `ISTMO_AUTO_PLUGINS` and
`ISTMO_AUTO_REMOTE`, parses each entry as a Rust path, and appends it to
whatever you wrote:

```rust
// Your source:
istmo::runtime!(
    plugins: [MyOwnClient],
);

// After expansion (conceptually):
pub fn __istmo_configure_runtime(init: RuntimeInit) -> RuntimeInit {
    init.expects::<MyOwnClient>()
        .expects::<::istmo_google_sign_in::SignInClient>()
        .expects::<::istmo_data_store::DataStoreClient>()
        .remotes::<::acme_model::LargeModelClient>()
        .finish()
}
```

A plugin listed both manually and auto-wired is declared once — the
runtime keeps its declarations in a set, so writing both is safe. The
expansion also names both variables through `option_env!`, so a changed
list recompiles the crate.

## What gets auto-wired

Only plugins that:

- Come from a **direct** Cargo dependency whose crate runs
  `istmo_build::emit()` and carries `links = "…"`.
- Have a `client_type` set in their `istmo.toml`.

Manifests the app passes in `AppOpts::extra_manifests` count as handed
over by a dependency, so they are auto-wired too.

Plugins without a `client_type` (shared types only, or a service /
background shim) are skipped — there is no client to declare.

## Deployment target

Each plugin has a `default_deployment` — `local` or `remote`. Auto-wiring
respects it:

- `local` → the client goes to `ISTMO_AUTO_PLUGINS` (`expects`).
- `remote` → the client goes to `ISTMO_AUTO_REMOTE` (`remotes`).

App-side `[[remote_override]]` flips a plugin's placement without
touching the plugin crate.

## Opting out

There is no "skip" flag — every plugin with a `client_type` among your
direct dependencies is declared. Every app-side entry point (`emit`,
`emit_with`, `emit_app`, `emit_app_with`) bakes `ISTMO_AUTO_PLUGINS` /
`ISTMO_AUTO_REMOTE`, so auto-wiring cannot be turned off from
`build.rs`. Declaring a plugin only lets the runtime call it; nothing is
constructed until you acquire its client. To keep a plugin out, drop
the dependency.

## Permissions, plist entries and shims

The app-side pass resolves the same wired set in-process, so an
auto-wired plugin contributes everything a listed one does: its
`android_permissions` to `android/.istmo/istmo-permissions.xml`, its
`[plugin.info_plist]` entries to the merged `Info.plist` fragment, and
its service / background shims. The wired set is the auto-wired clients
plus every client the `plugins:` and `remote:` sections of
`istmo::runtime!` name in `src/lib.rs` or `src/main.rs` (a trait in
`remote:` counts as its `<Trait>Client`). Listing an auto-wired plugin
in `runtime!` as well changes nothing.

## Debugging

Auto-wiring failures are usually one of:

- **Plugin has no `client_type`** — check the plugin's `istmo.toml`.
- **Plugin crate has no `links = "…"`** — Cargo only forwards build
  script metadata from crates that declare `links`.
- **The plugin is a transitive dependency** — `DEP_*` variables only
  reach direct dependents; add the plugin to your own `Cargo.toml`.
- **`istmo::runtime!` is expanded in a crate whose `build.rs` does not
  call `emit()`** — the env vars are set only for the crate whose build
  script prints them.

Print the effective wiring during the build:

```rust
// build.rs
fn main() {
    istmo_build::emit();

    let app = istmo_build::Manifest::from_path("istmo.toml").ok();
    let wiring = istmo_build::resolve_wiring(
        &istmo_build::collect_dep_manifests(),
        app.as_ref(),
    );
    println!("cargo::warning=local: {:?}", wiring.local_clients);
    println!("cargo::warning=remote: {:?}", wiring.remote_clients);
}
```

## Why env vars and not `inventory`?

`inventory` / `linkme` do link-time collection — every crate registers
into a global table via a linker section. It works, but:

- Static libraries (the iOS `staticlib`) can drop a registration the
  linker sees no reference to.
- Section handling differs per linker and platform.
- Env-var-driven `runtime!` is resolved entirely at `cargo build` time —
  no linker involvement.

The trade-off is that auto-wiring only sees plugins from **direct**
Cargo dependencies.

## Next

- Runtime setup shape: [Concepts → Architecture](/istmo/concepts/architecture/).
- Deploying to `:remote`: [Advanced → Remote process](/istmo/advanced/remote-process/).
