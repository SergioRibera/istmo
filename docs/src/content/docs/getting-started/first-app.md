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

The template needs `cargo-generate` 0.21 or newer.

## Generate a project

```bash
cargo generate --git https://github.com/SergioRibera/istmo-template --name my-app
```

Answer the prompts (project archetype `app` or `plugin`, bundle id,
target platforms, initial plugins, license, CI provider, UI framework,
istmo version) and `cd` into the new directory.

## What just happened

The template ships two archetypes — `app` and `plugin` — under nested
folders inside the template repo. Post-generate the hook **flattens the
chosen archetype into the project root**, so what you get is a flat
project layout:

```
my-app/
├── Cargo.toml               # the app crate
├── build.rs                 # one-liner: istmo_build::emit()
├── istmo.toml               # app id, name, version, icon, overrides
├── justfile                 # build / run / lint recipes
├── flake.nix                # optional Nix devshell (desktop targets)
├── src/
│   ├── lib.rs               # istmo::runtime!() + #[istmo::mobile_app] entry point
│   ├── app.rs               # shared app state and UI
│   ├── main.rs              # desktop entry point
│   └── desktop.rs           # desktop runtime (Rust hosts + emulator)
├── android/                 # Gradle project (drives cargo)
│   └── app/src/main/AndroidManifest.xml
└── ios/                     # xcodegen project (drives cargo)
    ├── project.yml
    └── <Name>App/main.swift
```

No `app/` or `plugin/` prefix directories — every file sits at the
project root. If you picked the `plugin` archetype instead, the tree is
similar but with `native/{android,ios}/` reference backends in place of
the platform project folders.

- `src/lib.rs` is your entry point. On Android the launcher activity,
  `dev.istmo.runtime.IstmoGameActivity`, loads the library and runs it;
  on iOS `main.swift` hands control to it through `IstmoApp.run()`.
- `build.rs` runs on every compile. Because you have both `android/`
  and `ios/` next to `Cargo.toml`, `istmo_build::emit()` additionally
  generates Kotlin/Swift dispatcher shims for each plugin your app
  depends on **and** an `IstmoPluginRegistry` for one-shot native
  registration.
- `istmo.toml` declares the app identity and any app-level overrides —
  see the [`[app]` reference](/istmo/build-scripts/istmo-toml-reference/#app).

## Auto-registration is on by default

The template ships no Kotlin activity and a one-line `main.swift`: the
runtime's entry points start `IstmoRuntime` and register every plugin
through the generated `IstmoPluginRegistry` before your Rust entry point
runs. On Android the manifest names the runtime's activity directly:

```xml
<!-- android/app/src/main/AndroidManifest.xml -->
<activity
    android:name="dev.istmo.runtime.IstmoGameActivity"
    android:exported="true">
    <meta-data android:name="android.app.lib_name" android:value="my_app" />
    <!-- MAIN / LAUNCHER intent filter -->
</activity>
```

Subclass `IstmoGameActivity` and point the manifest at your class only
when you need Kotlin code of your own.

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
live in `istmo.toml`; the `io.github.sergioribera.istmo` Gradle plugin and the
generated Xcode settings apply them, and the same Gradle plugin and Xcode
pre-build phase run cargo for you. If something is missing, `cargo
build` prints `istmo doctor:` warnings and `./gradlew istmoDoctor`
checks the toolchain. See
[Native projects](/istmo/build-scripts/native-projects/).

## Run it

### Android

The template ships no Gradle wrapper; create it once, then build and
install:

```bash
cd android
gradle wrapper
./gradlew installDebug
```

The `io.github.sergioribera.istmo` Gradle plugin invokes cargo to
produce a `.so` per ABI and packages it into the APK; Gradle installs
it on the connected device or emulator. `just build-android` builds the
debug APK.

### iOS

Run one `cargo build` (or `cargo check`) first: it writes
`ios/<Name>App/istmo-plugins.yml` (plugin sources, app identity, link
settings, the cargo pre-build phase), which `project.yml` includes,
and `ios/.istmo/`.

```bash
cargo check
cd ios
xcodegen generate
open <Name>App.xcodeproj
```

Xcode's pre-build phase invokes cargo to produce a `libmy_app.a`, which
is linked into the Swift app. `just build-ios` does the same for the
simulator.

### Desktop

```bash
just run    # cargo run
```

`src/desktop.rs` starts an in-process runtime: plugins with a desktop
backend run as Rust hosts, calls to the others fail with a log line
until you emulate them.

## Where to go next

- Add a plugin: [Writing your first plugin](/istmo/getting-started/first-plugin/).
- Understand the pieces: [Architecture](/istmo/concepts/architecture/).
- Ship a background service: [Services and workers](/istmo/writing-plugins/services-workers/).
