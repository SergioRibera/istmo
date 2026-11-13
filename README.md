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
- **Typed plugins.** `#[istmo::plugin] trait FooPlugin` declares an
  async surface; the macro emits `FooClient` (used from Rust) and
  `FooHost<Impl>` (used from Rust-hosted plugins) plus the wire codec.
  `istmo-build` regenerates matching `FooDispatcher.{kt,swift}` +
  `FooCodecs.{kt,swift}` in the consumer's `build.rs`.
- **Bincode over one wire.** Frames cross JNI / Swift FFI as opaque
  byte payloads; consumers never hand-write encode/decode.
- **Same runtime for every surface.** Desktop, Android, iOS all boot
  the same `Runtime`; only the transport module differs
  (`InlineMainThread`, `istmo-android`, `istmo-ios`).

## Consuming the framework

### Rust — Cargo

```toml
[dependencies]
istmo = { version = "*" }
```

The `istmo` facade re-exports:

- `istmo::core` — `Runtime`, `Envelope`, `Frame`, `NativeHandle`, wire codec.
- `istmo::plugin` / `istmo::message` / `istmo::service` / `istmo::worker` /
  `istmo::mobile_app` — proc-macros.
- `istmo::runtime!` — declarative wiring of the process-global runtime.

Platform-specific transports (`istmo-android`, `istmo-ios`) are pulled
in automatically by target-cfg. Community plugins ship as separate
crates (`istmo-google-sign-in`, `istmo-data-store`,
`istmo-live-activity`, `istmo-biometric`, `istmo-share`).

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
