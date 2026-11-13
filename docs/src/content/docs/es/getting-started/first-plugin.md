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
batería del dispositivo, y envía sus backends de Android e iOS dentro
del crate del plugin.

:::tip[Arranca desde el template]
`cargo generate --git https://github.com/SergioRibera/istmo-template`
con el arquetipo `plugin` genera todo lo de abajo — `Cargo.toml`,
`istmo.toml`, `build.rs`, el trait y backends de referencia en
`native/` — para un plugin de ejemplo stateful.
:::

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
edition = "2024"
rust-version = "1.89"
# Cargo solo reenvía la metadata del build script (el manifest y el
# contrato del plugin) a los dependientes de un crate que declara `links`.
links = "battery_plugin"

[dependencies]
# `istmo-core` + `istmo-macros` en lugar del facade `istmo`: el facade
# elige el sabor de activity Android, y esa es decisión de la app.
istmo-core   = "0.1.1-alpha.5"
istmo-macros = "0.1.1-alpha.5"
wincode      = { version = "0.6", default-features = false, features = ["derive", "std"] }

[build-dependencies]
istmo-build = "0.1.1-alpha.5"
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
id              = "myapp.battery"
client_type     = "::battery_plugin::BatteryClient"
# Paquete Kotlin de native/android/. Las apps generan ahí el
# dispatcher y los tipos de este plugin.
android_package = "com.myapp.battery"
```

`istmo.toml` es la única fuente de verdad para la identidad del plugin,
su path del cliente Rust, el paquete Kotlin de sus fuentes Android, y
cualquier dependencia Gradle o SwiftPM que necesite. Mantené
`android_package` separado del paquete propio de cualquier app (el
template usa `<bundle_id>.<crate>`): es de `istmo-build`. Ver la
[referencia del manifest](/istmo/es/build-scripts/istmo-toml-reference/).

## Escribe el trait

`plugins/battery/src/lib.rs`:

```rust
use istmo_macros::{message, plugin};

/// Lector de estado de batería.
///
/// Implementado nativamente — Android usa `BatteryManager`, iOS usa
/// `UIDevice.current.batteryLevel`.
#[plugin(name = "myapp.battery", crate = "::istmo_core")]
pub trait Battery {
    /// Porcentaje 0..=100 o -1 si el dispositivo no lo reporta.
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

`name` es el id del wire y coincide con `id` en `istmo.toml`. Como el
crate depende de `istmo-core` directamente, `crate = "::istmo_core"`
apunta el código generado ahí, y `wincode = "::wincode"` hace lo mismo
para los derives de los mensajes.

`#[plugin]` emite tres siblings en compile time:

- `Battery` — el trait que declaraste (con `async fn` desugared).
- `BatteryClient` — el wrapper caller-side de Rust, más un impl de
  `Plugin`.
- `BatteryHost<T: Battery>` — el dispatcher, usado cuando Rust
  hostea el plugin.

Para un plugin native-hosted usas `BatteryClient` desde Rust e
implementas el backend nativamente.

## Implementa el lado nativo

El build script de cada **app** que depende de este crate — no el de
este crate — genera un dispatcher Kotlin (`BatteryDispatcher.kt`, con
la interfaz `BatteryBackend` y el tipo `BatteryError`) en
`com.myapp.battery`, y un dispatcher Swift (`BatteryDispatcher.swift`)
en el target de la app. Los backends viven en el crate del plugin, al
lado de `istmo.toml`.

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

La fuente declara el paquete de `android_package`, así nombra el
`BatteryBackend` generado sin import y compila sea cual sea el paquete
Kotlin de la app. Solo los tipos del runtime (`BackendException`,
`IstmoRuntime`, …) vienen de `dev.istmo.runtime`. El plugin de Gradle
`io.github.sergioribera.istmo` compila `native/android/` en cada app
que depende del crate.

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

El fragmento xcodegen `istmo-plugins.yml` generado añade `native/ios/`
al target de la app.

## Cablea todo

Añade el crate del plugin a la app:

```toml
[dependencies]
battery-plugin = { path = "../plugins/battery" }
```

Eso es todo. El `build.rs` de la app lee el manifest del plugin, así
que `istmo::runtime!()` en el `src/lib.rs` de la app cablea
`BatteryClient` sin listarlo (ver
[auto-wiring](/istmo/es/advanced/auto-wiring/)), y el
`IstmoPluginRegistry` generado construye
`com.myapp.battery.BatteryBackendImpl(context)` en Android y
`BatteryBackendImpl()` en iOS.

Los entry points del template llaman al registry antes de que corra tu
entry point de Rust: el manifest nombra
`dev.istmo.runtime.IstmoGameActivity` como activity launcher, y
`main.swift` es una línea:

```swift
import IstmoRuntime

IstmoApp.run()
```

Una app con su propia activity Android llama a
`IstmoHost.onCreate(this)` antes de `super.onCreate`. Ver
[Auto-registración](/istmo/es/build-scripts/auto-register/) si
necesitás opt-out o manejar un constructor bespoke.

## Invócalo

En cualquier lugar de tu código Rust:

```rust
let battery = BatteryClient::from_runtime(&runtime)?;
let pct = battery.level().await?;
log::info!("battery = {pct}%");
```

Ese es el loop completo. Rust llamó un método de trait → Istmo
serializó el call → la plataforma corrió el código real → el resultado
volvió tipado.

## Siguientes pasos

- Aprende el split: [Native vs. Rust-hosted](/istmo/es/concepts/native-vs-rust-hosted/).
- Maneja eventos con streams: [Streams](/istmo/es/writing-plugins/streams/).
- Cancela calls en vuelo limpiamente: [Cancelación](/istmo/es/writing-plugins/cancellation/).
