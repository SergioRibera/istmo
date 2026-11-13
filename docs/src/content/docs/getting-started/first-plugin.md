---
title: Your first plugin
description: Wrap a native SDK behind a Rust trait, then call it from any target.
sidebar:
  order: 3
---

A "plugin" in Istmo is any Rust trait annotated with
`#[istmo::plugin]`. Depending on which side implements the trait — Rust
or the native platform — it is either **Rust-hosted** or
**native-hosted**. This walkthrough builds a small native-hosted plugin
that reads the device battery level, and ships its Android and iOS
backends inside the plugin crate.

:::tip[Start from the template]
`cargo generate --git https://github.com/SergioRibera/istmo-template`
with the `plugin` archetype scaffolds everything below — `Cargo.toml`,
`istmo.toml`, `build.rs`, the trait and reference backends under
`native/` — for a stateful example plugin.
:::

## Create the crate

Inside your workspace, create a new library crate:

```bash
cargo new --lib plugins/battery
```

Update `plugins/battery/Cargo.toml`:

```toml
[package]
name = "battery-plugin"
version = "0.1.0"
edition = "2024"
rust-version = "1.89"
# Cargo only forwards build-script metadata (the plugin manifest and
# contract) to dependents of a crate that declares `links`.
links = "battery_plugin"

[dependencies]
# `istmo-core` + `istmo-macros` instead of the `istmo` facade: the
# facade picks the Android activity flavour, which is the app's choice.
istmo-core   = "0.1.1-alpha.5"
istmo-macros = "0.1.1-alpha.5"
wincode      = { version = "0.6", default-features = false, features = ["derive", "std"] }

[build-dependencies]
istmo-build = "0.1.1-alpha.5"
```

Add a build script:

```rust
// plugins/battery/build.rs
fn main() {
    istmo_build::emit();
}
```

## Declare the plugin

`plugins/battery/istmo.toml`:

```toml
[plugin]
id              = "myapp.battery"
client_type     = "::battery_plugin::BatteryClient"
# Kotlin package of native/android/. Apps generate this plugin's
# dispatcher and types into it.
android_package = "com.myapp.battery"
```

`istmo.toml` is the single source of truth for the plugin's identity,
its Rust client path, the Kotlin package of its Android sources, and
any Gradle or SwiftPM dependencies it needs. Keep `android_package`
apart from every app's own package (the template uses
`<bundle_id>.<crate>`): `istmo-build` owns it. See the
[manifest reference](/istmo/build-scripts/istmo-toml-reference/).

## Write the trait

`plugins/battery/src/lib.rs`:

```rust
use istmo_macros::{message, plugin};

/// Battery status reader.
///
/// Implemented natively — Android uses `BatteryManager`, iOS uses
/// `UIDevice.current.batteryLevel`.
#[plugin(name = "myapp.battery", crate = "::istmo_core")]
pub trait Battery {
    /// Percentage 0..=100 or -1 if the device does not report it.
    async fn level(&self) -> Result<i32, BatteryError>;
}

#[message(wincode = "::wincode")]
#[derive(Debug, Clone)]
pub struct BatteryError {
    pub reason: String,
}

impl core::fmt::Display for BatteryError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "battery error: {}", self.reason)
    }
}

impl std::error::Error for BatteryError {}
```

`name` is the wire id and matches `id` in `istmo.toml`. Because the
crate depends on `istmo-core` directly, `crate = "::istmo_core"` points
the generated code at it, and `wincode = "::wincode"` does the same for
the message derives.

`#[plugin]` emits three siblings at compile time:

- `Battery` — the trait you just declared (with `async fn` desugared).
- `BatteryClient` — the Rust caller-side wrapper, plus a `Plugin` impl.
- `BatteryHost<T: Battery>` — the dispatcher, used when Rust hosts the
  plugin.

For a native-hosted plugin, you use `BatteryClient` from Rust and
implement the backend natively.

## Implement the native side

The build script of every **app** that depends on this crate — not this
crate — generates a Kotlin dispatcher (`BatteryDispatcher.kt`, with the
`BatteryBackend` interface and the `BatteryError` type) into
`com.myapp.battery`, and a Swift dispatcher (`BatteryDispatcher.swift`)
into the app target. The backends live in the plugin crate, next to
`istmo.toml`.

### Android

`plugins/battery/native/android/BatteryBackendImpl.kt`:

```kotlin
package com.myapp.battery

import android.content.Context
import android.os.BatteryManager

class BatteryBackendImpl(private val ctx: Context) : BatteryBackend {
    override suspend fun level(): Int {
        val bm = ctx.getSystemService(Context.BATTERY_SERVICE) as BatteryManager
        return bm.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY)
    }
}
```

The source declares the `android_package` package, so it names the
generated `BatteryBackend` without an import and compiles whatever
Kotlin package the app uses. Only runtime types (`BackendException`,
`IstmoRuntime`, …) come from `dev.istmo.runtime`. The
`io.github.sergioribera.istmo` Gradle plugin compiles `native/android/`
into every app that depends on the crate.

### iOS

`plugins/battery/native/ios/BatteryBackendImpl.swift`:

```swift
import UIKit

final class BatteryBackendImpl: BatteryBackend {
    func level() async throws -> Int32 {
        await MainActor.run { UIDevice.current.isBatteryMonitoringEnabled = true }
        let level = await MainActor.run { UIDevice.current.batteryLevel }
        return Int32(level < 0 ? -1 : level * 100)
    }
}
```

The generated `istmo-plugins.yml` xcodegen fragment adds `native/ios/`
to the app target.

## Wire it up

Add the plugin crate to the app:

```toml
[dependencies]
battery-plugin = { path = "../plugins/battery" }
```

That is all. The app's `build.rs` reads the plugin's manifest, so
`istmo::runtime!()` in the app's `src/lib.rs` wires `BatteryClient`
without listing it (see [auto-wiring](/istmo/advanced/auto-wiring/)),
and the generated `IstmoPluginRegistry` constructs
`com.myapp.battery.BatteryBackendImpl(context)` on Android and
`BatteryBackendImpl()` on iOS.

The template's entry points call the registry before your Rust entry
point runs: the manifest names `dev.istmo.runtime.IstmoGameActivity`
as the launcher activity, and `main.swift` is one line:

```swift
import IstmoRuntime

IstmoApp.run()
```

An app with its own Android activity calls `IstmoHost.onCreate(this)`
before `super.onCreate`. See
[Auto-registration](/istmo/build-scripts/auto-register/) to opt out or
handle a bespoke constructor.

## Call it

Anywhere in your Rust code:

```rust
let battery = BatteryClient::from_runtime(&runtime)?;
let pct = battery.level().await?;
log::info!("battery = {pct}%");
```

That's the whole loop. Rust called a trait method → Istmo serialized
the call → the platform ran the real code → the result came back typed.

## Next steps

- Learn the split: [Native vs. Rust-hosted plugins](/istmo/concepts/native-vs-rust-hosted/).
- Handle events with streams: [Streams](/istmo/writing-plugins/streams/).
- Cancel in-flight calls cleanly: [Cancellation](/istmo/writing-plugins/cancellation/).
