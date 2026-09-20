---
title: Plugin Rust-hosted
description: Envía la implementación del trait en Rust; Kotlin y Swift opcionalmente llaman hacia adentro.
sidebar:
  order: 1
---

Un plugin Rust-hosted pone el trabajo real del trait en Rust. El lado
nativo o no toca el plugin (código Rust lo llama), o sostiene un
`<T>Client` generado y llama hacia adentro cuando necesita un
resultado.

## Cuándo elegirlo

- Computación pura — hashing, parsing, criptografía, procesamiento de
  imágenes.
- Lógica de negocio que quieres byte-idéntica en cada plataforma.
- Código que ya tienes en Rust que preferirías dejar quieto.

Si tu plugin necesita hablar con un SDK nativo, pedir permisos, o
tocar el main thread de la plataforma, probablemente quieras un
[plugin native-hosted](/istmo/es/writing-plugins/native-hosted/) en su
lugar.

## Skeleton

```rust
// crates/image-hasher/src/lib.rs
use istmo::plugin;

#[plugin]
pub trait ImageHasher {
    async fn perceptual_hash(&self, bytes: Vec<u8>) -> Result<u64, HashError>;
    async fn dimensions(&self, bytes: Vec<u8>) -> Result<Dimensions, HashError>;
}

#[istmo::message]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

#[istmo::message]
pub struct HashError {
    pub reason: String,
}

impl core::fmt::Display for HashError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "hasher error: {}", self.reason)
    }
}
impl std::error::Error for HashError {}
```

`istmo.toml`:

```toml
[plugin]
id          = "acme.image_hasher"
client_type = "::image_hasher::ImageHasherClient"
```

`build.rs`:

```rust
fn main() { istmo_build::emit(); }
```

## Implementa el trait

Justo al lado de la definición del trait:

```rust
pub struct ImageHasherImpl;

impl ImageHasher for ImageHasherImpl {
    async fn perceptual_hash(&self, bytes: Vec<u8>) -> Result<u64, HashError> {
        // tu lógica de hashing real
        Ok(compute_phash(&bytes))
    }

    async fn dimensions(&self, bytes: Vec<u8>) -> Result<Dimensions, HashError> {
        let (w, h) = probe(&bytes).map_err(|e| HashError {
            reason: e.to_string(),
        })?;
        Ok(Dimensions { width: w, height: h })
    }
}
```

Nota: `#[istmo::plugin]` desugara cada `async fn` del trait a
`impl Future + Send + '_`, así que implementás el trait con
`async fn` plano en Rust 1.75+. No hace falta `#[async_trait]`.

## Cablea el host al runtime

```rust
use image_hasher::{ImageHasherHost, ImageHasherImpl};

istmo::runtime!(
    hosts: [ImageHasherHost::new(ImageHasherImpl)],
);
```

`<T>Host` lo emite la macro del plugin. `<T>Host::new(impl)`
construye el dispatcher y lo registra con el runtime en init.

## Invócalo desde Rust

En cualquier lugar donde tengas un `Arc<Runtime>`:

```rust
use image_hasher::ImageHasherClient;

let hasher = ImageHasherClient::from_runtime(&runtime)?;
let phash = hasher.perceptual_hash(pixels).await?;
```

## Opcionalmente, invócalo desde Kotlin o Swift

Si el lado nativo necesita invocar el plugin, opt-in con un override
app-side:

```toml
# istmo.toml (en la raíz del crate app)
[[app.plugin]]
id   = "acme.image_hasher"
role = "client"
```

El próximo `cargo build` regenera archivos `<T>Client.kt` /
`<T>Client.swift`. Uso desde Kotlin:

```kotlin
val hasher = ImageHasherClient(ImageHasherCodecsImpl())
val phash = hasher.perceptualHash(bytes)
```

## Testing

Los plugins Rust-hosted son trivialmente unit-testeables porque todo
es sólo Rust async:

```rust
#[tokio::test]
async fn hashes_a_png() {
    let png = include_bytes!("../tests/sample.png").to_vec();
    let out = ImageHasherImpl.perceptual_hash(png).await.unwrap();
    assert_ne!(out, 0);
}
```

Para tests de integración, levanta un runtime con
`Runtime::new_isolated()` y llama al cliente como tu app lo haría.

## Siguiente

- Añadir un stream: [Streams](/istmo/es/writing-plugins/streams/).
- Cancel cooperativo: [Cancelación](/istmo/es/writing-plugins/cancellation/).
- Si de verdad quieres hablar con Kotlin/Swift: [Plugin native-hosted](/istmo/es/writing-plugins/native-hosted/).
