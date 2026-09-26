---
title: Your first app
description: Scaffold a full-Rust mobile app that runs on Android and iOS with a single command.
sidebar:
  order: 2
---

The fastest path to a working app is
[`cargo-generate`](https://github.com/cargo-generate/cargo-generate)
against the official template. The template scaffolds a Rust crate,
Gradle Android project, and Xcode iOS project all wired to talk to each
other.

## Install cargo-generate

```bash
cargo install cargo-generate
```

## Generate a project

```bash
cargo generate --git https://github.com/sergioribera/istmo-template --name my-app
```

Answer the prompts (project archetype `app` or `plugin`, bundle id,
target platforms, initial plugins, license, CI provider, UI framework)
and `cd` into the new directory.

## What just happened

The template ships two archetypes — `app` and `plugin` — under nested
folders inside the template repo. Post-generate the hook **flattens the
chosen archetype into the project root**, so what you get is a flat
project layout:

```
my-app/
├── Cargo.toml               # the app crate
├── build.rs                 # one-liner: istmo_build::emit()
├── istmo.toml               # app-side codegen config
├── flake.nix                # optional Nix devshell (desktop targets)
├── src/
│   ├── lib.rs               # #[istmo::mobile_app] entry point
│   ├── app.rs               # shared UI code
│   └── main.rs              # desktop entry point
├── android/                 # Gradle project (drives cargo)
│   └── app/src/main/kotlin/<pkg>/<Name>Activity.kt
└── ios/                     # Xcode project (drives cargo)
    └── <Name>App/main.swift
```

No `app/` or `plugin/` prefix directories — every file sits at the
project root. If you picked the `plugin` archetype instead, the tree is
similar but with `native/{android,ios}/` reference backends in place of
the platform project folders.

- `src/lib.rs` is your entry point. On Android it is invoked from
  `NativeActivity`; on iOS from a Swift `App` struct.
- `build.rs` runs on every compile. Because you have both `android/`
  and `ios/` next to `Cargo.toml`, `istmo_build::emit()` additionally
  generates Kotlin/Swift dispatcher shims for each plugin your app
  depends on **and** an `IstmoPluginRegistry` for one-shot native
  registration.
- `istmo.toml` declares any app-level overrides — see the
  [`[app]` reference](/istmo/build-scripts/istmo-toml-reference/#app).

## Auto-registration is on by default

The Android activity and the iOS `main.swift` are one-liners: the
runtime's entry points start `IstmoRuntime` and register every plugin
through the generated `IstmoPluginRegistry` before your Rust entry point
runs:

```kotlin
// android/app/src/main/kotlin/<pkg>/<Name>Activity.kt
class MainActivity : IstmoGameActivity()
```

```swift
// ios/<Name>App/main.swift
import IstmoRuntime

IstmoApp.run()
```

Every plugin whose `istmo.toml` declares `auto_register = true` (the
default) is registered, including plugins whose Android backend needs
the host activity. See
[Auto-registration](/istmo/build-scripts/auto-register/) for the
mechanism.

## One source of truth

The app id, name, version, build number, icon and minimum OS versions
live in `istmo.toml`; the `dev.istmo.app` Gradle plugin and the
generated Xcode settings apply them, and the same Gradle plugin and Xcode
pre-build phase run cargo for you. If something is missing, `cargo
build` prints `istmo doctor:` warnings and `./gradlew istmoDoctor`
checks the toolchain. See
[Native projects](/istmo/build-scripts/native-projects/).

## Run it

### Android

```bash
cd android
./gradlew installDebug
```

Gradle invokes cargo (via a build task) to produce a `.so` per ABI,
copies it into the APK, and installs to the connected device or
emulator.

### iOS

```bash
cd ios
xcodegen
open <Name>App.xcodeproj
```

Xcode's build phase invokes cargo to produce a `libmy_app.a`, which is
linked into the Swift app.

## Where to go next

- Add a plugin: [Writing your first plugin](/istmo/getting-started/first-plugin/).
- Understand the pieces: [Architecture](/istmo/concepts/architecture/).
- Ship a background service: [Services and workers](/istmo/writing-plugins/services-workers/).
