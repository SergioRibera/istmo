---
title: Tu primer plugin
description: Envuelve un SDK nativo detrás de un trait Rust, luego invócalo desde cualquier target.
sidebar:
  order: 3
---

Un "plugin" en Istmo es cualquier trait Rust anotado con
`#[istmo::plugin]`. Dependiendo de qué lado implementa el trait — Rust
o la plataforma nativa — es **Rust-hosted** o **native-hosted**. Este
walkthrough construye un plugin native-hosted que lee el nivel de
batería del dispositivo.

## Crea el crate

Dentro de tu workspace, crea un crate library nuevo:

```bash
cargo new --lib plugins/battery
```

Actualiza `plugins/battery/Cargo.toml`:

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

Añade un build script:

```rust
// plugins/battery/build.rs
fn main() {
    istmo_build::emit();
}
```

## Declara el plugin

`plugins/battery/istmo.toml`:

```toml
[plugin]
id          = "myapp.battery"
client_type = "::battery_plugin::BatteryClient"
```

`istmo.toml` es la única fuente de verdad para la identidad del plugin,
su path del cliente Rust, y cualquier dependencia Gradle o SwiftPM que
necesite. Ver la [referencia del manifest](/istmo/es/build-scripts/istmo-toml-reference/).

## Escribe el trait

`plugins/battery/src/lib.rs`:

```rust
use istmo::plugin;

/// Lector de estado de batería.
///
/// Implementado nativamente — Android usa `BatteryManager`, iOS usa
/// `UIDevice.current.batteryLevel`.
#[plugin]
pub trait Battery {
    /// Porcentaje 0..=100 o -1 si el dispositivo no lo reporta.
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

`#[istmo::plugin]` emite tres siblings en compile time:

- `Battery` — el trait que declaraste (con `async fn` desugared).
- `BatteryClient` — el wrapper caller-side de Rust, más un impl de
  `Plugin`.
- `BatteryHost<T: Battery>` — el dispatcher, usado cuando Rust
  hostea el plugin.

Para un plugin native-hosted usas `BatteryClient` desde Rust e
implementas el backend nativamente.

## Implementa el lado nativo

El build script en tu **app** — no en este crate — regenerará un
dispatcher Kotlin (`BatteryDispatcher.kt`) y un dispatcher Swift
(`BatteryDispatcher.swift`) la próxima vez que corra. Cada uno expone
una interfaz para que la implementes.

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

## Cablea todo

En el `src/lib.rs` de tu app:

```rust
use battery_plugin::BatteryClient;

istmo::runtime!(
    plugins: [BatteryClient],
);
```

La macro `istmo::runtime!` acepta una lista de clients separados por
coma que planeas usar. También puedes dejarla auto-descubrir plugins
desde tu `Cargo.toml` — ver [auto-wiring](/istmo/es/advanced/auto-wiring/).

Registra el backend nativo desde tu Activity / App. `istmo-build`
genera una sola llamada `IstmoPluginRegistry.registerAll(...)` que
cablea todos los plugins elegibles — no escribís a mano la
registración del dispatcher.

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

Ver [Auto-registración](/istmo/es/build-scripts/auto-register/) si necesitás
opt-out o manejar un constructor bespoke.

## Invócalo

En cualquier lugar de tu código Rust:

```rust
let battery = BatteryClient::from_runtime(&runtime)?;
let pct = battery.level().await?;
tracing::info!("battery = {pct}%");
```

Ese es el loop completo. Rust llamó un método de trait → Istmo
serializó el call → la plataforma corrió el código real → el resultado
volvió tipado.

## Siguientes pasos

- Aprende el split: [Native vs. Rust-hosted](/istmo/es/concepts/native-vs-rust-hosted/).
- Maneja eventos con streams: [Streams](/istmo/es/writing-plugins/streams/).
- Cancela calls en vuelo limpiamente: [Cancelación](/istmo/es/writing-plugins/cancellation/).
