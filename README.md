# istmo

Rust ↔ mobile native interop framework. Write shared Rust code, ship
it to Android + iOS through a typed plugin contract — no
per-plugin JNI/FFI trampolines, no per-platform runtime forks, one
version cutting across every language surface.

**Status:** early alpha. API shape churns between minor releases; wire
protocol is versioned and validated at runtime.

## What it does

- **One Rust codebase**, one cargo build per platform target
  (`aarch64-linux-android`, `aarch64-apple-ios`, plus desktop targets
  as a plain `cargo run`).
- **Typed plugins.** `#[istmo::plugin] trait Foo` declares an async
  surface; the macro emits `FooClient` (called from Rust) and
  `FooHost<Impl>` (a Rust implementation) plus the wire codec. The
  consuming app's `build.rs` generates the matching
  `FooDispatcher` / `FooTypes` / `FooCodecsImpl` in Kotlin and Swift,
  and an `IstmoPluginRegistry` that registers every native backend.
- **One wire codec.** Payloads are wincode-encoded, laid out like
  bincode 2 `standard()`, and cross JNI / Swift FFI as opaque bytes;
  consumers never hand-write encode/decode.
- **Same runtime for every surface.** Desktop, Android, iOS all boot
  the same `Runtime`; only the transport module differs
  (`InlineMainThread`, `istmo-android`, `istmo-ios`).

## Consuming the framework

### Rust — Cargo

```toml
[dependencies]
istmo = { version = "0.1.1-alpha.5", default-features = false, features = ["game-activity"] }
```

Istmo ships as pre-releases, so name the exact version (a `"0.1"` or
`"*"` requirement does not match `0.1.1-alpha.5`). `game-activity`
builds the Android side for `IstmoGameActivity`; the default
`native-activity` targets `IstmoNativeActivity`.

The `istmo` facade re-exports:

- everything in `istmo-core` at its root — `Runtime`, `Envelope`,
  `Frame`, `NativeHandle`, `CancelToken`, `TypedStream`, the `codec`
  module.
- `istmo::plugin` / `message` / `stream` / `owned` / `handle` /
  `redact` / `service` / `worker` / `mobile_app` — attribute macros.
- `istmo::runtime!` — declarative wiring of the process-global
  runtime; `istmo::app_icon!` — the app icon as PNG bytes.
- `istmo::plugins` — first-party plugin surfaces (lifecycle, deep
  links, safe area, system info, …).

Platform-specific transports (`istmo-android`, `istmo-ios`) are pulled
in automatically by target cfg. First-party plugins ship as separate
crates (`istmo-barcode-scanner`, `istmo-biometric`, `istmo-camera`,
`istmo-data-store`, `istmo-file-picker`, `istmo-google-sign-in`,
`istmo-live-activity`, `istmo-opener`, `istmo-pen`, `istmo-share`)
at the same version. New projects start from
[`istmo-template`](https://github.com/SergioRibera/istmo-template)
(`cargo generate --git https://github.com/SergioRibera/istmo-template`).

### Kotlin — Gradle (Android)

The Kotlin runtime (`io.github.sergioribera:istmo-runtime`) and the
`io.github.sergioribera.istmo` Gradle plugin
(`io.github.sergioribera:istmo-gradle-plugin`) are published to Maven
Central — no extra repository or credentials needed. Consumer setup
lives in [`runtime/README.md`](runtime/README.md).

```kotlin
// settings.gradle.kts
pluginManagement {
    repositories { google(); mavenCentral(); gradlePluginPortal() }
}
dependencyResolutionManagement {
    repositories { google(); mavenCentral() }
}

// app/build.gradle.kts
plugins {
    id("com.android.application")
    id("io.github.sergioribera.istmo") version "0.1.1-alpha.5"
}

dependencies {
    implementation("io.github.sergioribera:istmo-runtime:0.1.1-alpha.5")
}
```

### Swift — SPM (iOS / macOS)

```swift
dependencies: [
    .package(url: "https://github.com/SergioRibera/istmo.git", from: "0.1.1-alpha.5"),
],
targets: [
    .target(
        name: "MyApp",
        dependencies: [.product(name: "IstmoRuntime", package: "istmo")]
    ),
],
```

## License

Dual-licensed under **MIT OR Apache-2.0**, at the consumer's option.
