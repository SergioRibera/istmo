---
title: Install
description: Set up your Rust toolchain and platform SDKs to build Istmo apps and plugins.
sidebar:
  order: 1
---

Istmo runs on stable Rust and does not require any nightly features. What
you install beyond `cargo` depends on which platforms you plan to
target.

## Prerequisites

- **Rust 1.80 or newer.** Install via [rustup](https://rustup.rs). Add
  the Android or iOS targets you plan to use:
  ```bash
  rustup target add aarch64-linux-android armv7-linux-androideabi \
      x86_64-linux-android i686-linux-android
  rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
  ```
- **Android** — a working Android SDK 34+ and NDK r26+. Gradle 8.6 or
  newer. The Android build is driven by Gradle; the Rust cdylib is built
  from a Gradle task, so you do not need `cargo-ndk`.
- **iOS** — Xcode 15 or newer, `xcodegen` (installed with `brew install
  xcodegen`). The Rust side compiles as a `staticlib` and is linked into
  the Swift app.
- **Desktop** (optional) — the Nix flake at `flake.nix` provisions an
  `egui`-ready shell (Wayland, X11, mesa, alsa) so you can iterate on
  UI without a device attached.

## Add Istmo to a Rust workspace

Add the facade crate to your `Cargo.toml`. Every other crate you need
re-exports from it.

```toml
[dependencies]
istmo = "0.1"
```

If you use official plugins, add them alongside:

```toml
[dependencies]
istmo               = "0.1"
istmo-data-store    = "0.1"     # key/value storage
istmo-google-sign-in = "0.1"    # OAuth sign-in
istmo-live-activity = "0.1"    # iOS Live Activities + Android ongoing notif.
```

Every crate in the Istmo ecosystem shares one workspace version; you
never mix incompatible releases in the same tree.

## Build script wire-up

Every `build.rs` — plugin or app — is a one-liner:

```rust
fn main() {
    istmo_build::emit();
}
```

Add `istmo-build` to your `[build-dependencies]`:

```toml
[build-dependencies]
istmo-build = "0.1"
```

`emit()` figures out whether you're building a plugin, an app, or both.
See [`emit()` overview](/istmo/build-scripts/emit-overview/) for the full
picture.

## Next steps

- Scaffold your first app with the [template repo](/istmo/getting-started/first-app/).
- Understand what "plugin" means in Istmo through the [architecture
  concept guide](/istmo/concepts/architecture/).
- Skim the [`istmo.toml` reference](/istmo/build-scripts/istmo-toml-reference/)
  before you name your first plugin.
