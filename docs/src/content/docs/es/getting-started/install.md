---
title: Instalación
description: Configura tu toolchain de Rust y los SDKs de plataforma para construir apps y plugins Istmo.
sidebar:
  order: 1
---

Istmo corre en Rust estable y no requiere nightly. Lo que instalas más
allá de `cargo` depende de qué plataformas planeas soportar.

## Prerrequisitos

- **Rust 1.89 o mayor** (el codec del wire, wincode, lo necesita). Instálalo con [rustup](https://rustup.rs).
  Agrega los targets de Android o iOS que vayas a usar:
  ```bash
  rustup target add aarch64-linux-android armv7-linux-androideabi \
      x86_64-linux-android i686-linux-android
  rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
  ```
- **Android** — Android SDK 34+ y NDK r26+. Gradle 8.6 o mayor. El
  build de Android está manejado por Gradle; el cdylib de Rust se
  construye desde un task de Gradle, así que **no** necesitas
  `cargo-ndk`.
- **iOS** — Xcode 15 o mayor, `xcodegen` (`brew install xcodegen`).
  Rust compila como `staticlib` y se linkea dentro de la app Swift.
- **Desktop** (opcional) — el flake de Nix en `flake.nix` provisiona
  un shell listo para `egui` (Wayland, X11, mesa, alsa) para iterar
  sobre UI sin dispositivo conectado.

## Añadir Istmo a un workspace Rust

Añade el crate facade a tu `Cargo.toml`. Todos los demás crates que
necesites se re-exportan desde él.

```toml
[dependencies]
istmo = { version = "0.1.1-alpha.5", default-features = false, features = ["game-activity"] }
```

Istmo se publica como pre-release, así que nombra la versión exacta:
un requisito `"0.1"` a secas no matchea `0.1.1-alpha.5`. La feature
`game-activity` compila el lado Android para `IstmoGameActivity` (una
`ComponentActivity`, que necesitan plugins como sign-in, share o
biometric); la default `native-activity` apunta a `IstmoNativeActivity`.

Si usas plugins oficiales, agrégalos al lado:

```toml
[dependencies]
istmo-data-store     = "0.1.1-alpha.5"   # storage key/value
istmo-google-sign-in = "0.1.1-alpha.5"   # OAuth
istmo-live-activity  = "0.1.1-alpha.5"   # Live Activities iOS + notif. persistente Android
```

Todos los crates del ecosistema Istmo comparten una única versión de
workspace — igual que el artefacto Maven
`io.github.sergioribera:istmo-runtime`, el plugin de Gradle
`io.github.sergioribera.istmo` y el tag del paquete Swift
`IstmoRuntime` — así nunca mezclas releases incompatibles en el mismo
árbol.

## Wire-up del build script

Todo `build.rs` — plugin o app — es un one-liner:

```rust
fn main() {
    istmo_build::emit();
}
```

Añade `istmo-build` a tus `[build-dependencies]`:

```toml
[build-dependencies]
istmo-build = "0.1.1-alpha.5"
```

`emit()` decide si estás construyendo un plugin, una app, o ambos. Ver
[Vista general de `emit()`](/istmo/es/build-scripts/emit-overview/) para el
detalle.

## Siguientes pasos

- Bootstrap tu primera app con el [repo template](/istmo/es/getting-started/first-app/).
- Entiende qué significa "plugin" en Istmo con la [guía de conceptos
  arquitecturales](/istmo/es/concepts/architecture/).
- Da un vistazo a la [referencia de `istmo.toml`](/istmo/es/build-scripts/istmo-toml-reference/)
  antes de nombrar tu primer plugin.
