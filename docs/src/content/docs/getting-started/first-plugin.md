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
that reads the device battery level.

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
edition = "2021"

[lib]
crate-type = ["rlib"]

[dependencies]
istmo = "0.1"
bincode = "2"

[build-dependencies]
istmo-build = "0.1"
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
id          = "myapp.battery"
client_type = "::battery_plugin::BatteryClient"
```

`istmo.toml` is the single source of truth for the plugin's identity,
its Rust client path, and any Gradle or SwiftPM dependencies it needs.
See the [manifest reference](/build-scripts/istmo-toml-reference/).

## Write the trait

`plugins/battery/src/lib.rs`:

```rust
use istmo::plugin;

/// Battery status reader.
///
/// Implemented natively — Android uses `BatteryManager`, iOS uses
/// `UIDevice.current.batteryLevel`.
#[plugin]
pub trait Battery {
    /// Percentage 0..=100 or -1 if the device does not report it.
    async fn level(&self) -> Result<i32, BatteryError>;
}

#[istmo::message]
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

`#[istmo::plugin]` emits three siblings at compile time:

- `Battery` — the trait you just declared (with `async fn` desugared).
- `BatteryClient` — the Rust caller-side wrapper, plus a `Plugin` impl.
- `BatteryHost<T: Battery>` — the dispatcher, used when Rust hosts the
  plugin.

For a native-hosted plugin, you use `BatteryClient` from Rust and
implement the backend natively.

## Implement the native side

The build script at your **app** ­— not this crate — will regenerate a
Kotlin dispatcher (`BatteryDispatcher.kt`) and a Swift dispatcher
(`BatteryDispatcher.swift`) the next time it runs. Each one exposes an
interface for you to implement.

### Android

```kotlin
package com.myapp.gen

import android.content.Context
import android.os.BatteryManager

class BatteryBackendImpl(private val ctx: Context) : BatteryBackend {
    override suspend fun level(): Int {
        val bm = ctx.getSystemService(Context.BATTERY_SERVICE) as BatteryManager
        return bm.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY)
    }
}
```

### iOS

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

## Wire it up

In your app's `src/lib.rs`:

```rust
use battery_plugin::BatteryClient;

istmo::runtime!(
    plugins: [BatteryClient],
);
```

The `istmo::runtime!` macro accepts a comma-separated list of clients
you plan to call. You can also let it auto-discover plugins from your
`Cargo.toml` — see [auto-wiring](/advanced/auto-wiring/).

Register the native backend from your Activity / App. `istmo-build`
generates a single `IstmoPluginRegistry.registerAll(...)` call that
wires every eligible plugin — you don't hand-write the dispatcher
registration.

```kotlin
// Android — Activity onCreate
override fun onCreate(savedInstanceState: Bundle?) {
    IstmoRuntime.instance.start(this)
    IstmoPluginRegistry.registerAll(applicationContext)
    super.onCreate(savedInstanceState)
}
```

```swift
// iOS — App init
@main
struct MyApp: App {
    init() {
        IstmoRuntime.shared.start()
        IstmoPluginRegistry.registerAll()
    }
}
```

See [Auto-registration](/build-scripts/auto-register/) if you need to
opt out or handle a bespoke constructor.

## Call it

Anywhere in your Rust code:

```rust
let battery = BatteryClient::from_runtime(&runtime)?;
let pct = battery.level().await?;
tracing::info!("battery = {pct}%");
```

That's the whole loop. Rust called a trait method → Istmo serialized
the call → the platform ran the real code → the result came back typed.

## Next steps

- Learn the split: [Native vs. Rust-hosted plugins](/concepts/native-vs-rust-hosted/).
- Handle events with streams: [Streams](/writing-plugins/streams/).
- Cancel in-flight calls cleanly: [Cancellation](/writing-plugins/cancellation/).
