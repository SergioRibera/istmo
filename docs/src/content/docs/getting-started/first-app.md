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
  [`[app]` reference](/build-scripts/istmo-toml-reference/#app).

## Auto-registration is on by default

The generated Android `Activity` and iOS `main.swift` already call
`IstmoPluginRegistry.registerAll(...)` for you:

```kotlin
// android/app/src/main/kotlin/<pkg>/<Name>Activity.kt
override fun onCreate(savedInstanceState: Bundle?) {
    IstmoRuntime.instance.start(this)
    IstmoPluginRegistry.registerAll(applicationContext)
    super.onCreate(savedInstanceState)
}
```

```swift
// ios/<Name>App/main.swift
try IstmoRuntime.shared.start()
IstmoPluginRegistry.registerAll()
_ = istmo_run_ios()
```

Every plugin whose `istmo.toml` declares `auto_register = true` (the
default) is registered by that single call. See
[Auto-registration](/build-scripts/auto-register/) for the mechanism.

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

- Add a plugin: [Writing your first plugin](/getting-started/first-plugin/).
- Understand the pieces: [Architecture](/concepts/architecture/).
- Ship a background service: [Services and workers](/writing-plugins/services-workers/).
