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

:::note[Facade o crates core]
Los snippets de estas páginas usan el facade `istmo`, como lo hace el
código dentro de un crate de app. Un crate de plugin independiente
depende de `istmo-core` + `istmo-macros` (el facade elige el sabor de
activity Android, que es decisión de la app) y pasa
`crate = "::istmo_core"` a `#[plugin]` y `wincode = "::wincode"` a
`#[message]` — ver
[Tu primer plugin](/istmo/es/getting-started/first-plugin/).
:::

## Skeleton

```rust
// image-hasher/src/lib.rs
use istmo::plugin;

#[plugin(name = "acme.image_hasher")]
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
#[derive(Debug)]
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

El tipo de error de un retorno `Result` debe implementar
`std::error::Error` (de ahí el derive de `Debug`); la macro lo
comprueba.

`istmo.toml`:

```toml
[plugin]
id          = "acme.image_hasher"
client_type = "::image_hasher::ImageHasherClient"
# Nada nativo implementa este plugin: lo deja fuera del
# IstmoPluginRegistry generado de las apps, que si no construiría un
# `ImageHasherBackendImpl` en Android e iOS.
auto_register = false
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
        // tu lógica real de hashing
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

`#[istmo::plugin]` convierte cada `async fn` del trait en
`impl Future + Send + '_`, así que lo implementas con `async fn` plano.
No hace falta `#[async_trait]`.

## Conecta el host al runtime

En el crate de la app:

```rust
istmo::runtime!(
    hosts: [image_hasher::ImageHasher => image_hasher::ImageHasherImpl],
);
```

Cada entrada de `hosts:` es `Trait => expresión`: la macro registra
`<Trait>Host::new(expresión)`, así que `image_hasher::ImageHasherHost`
tiene que resolverse. El cliente sale del `client_type` del plugin vía
[auto-wiring](/istmo/es/advanced/auto-wiring/). Un plugin declarado
dentro del crate de la app no tiene manifest, así que lista también su
cliente — `plugins: [ImageHasherClient]` — o las llamadas fallan con
`IstmoError::PluginNotDeclared`.

## Llámalo desde Rust

En cualquier lugar donde tengas un `Arc<Runtime>`:

```rust
use image_hasher::ImageHasherClient;

let hasher = ImageHasherClient::from_runtime(&runtime)?;
let phash = hasher.perceptual_hash(pixels).await?;
```

Cada método del cliente retorna `Result<T, IstmoError>`. Un error de
dominio (`Err(HashError)` desde la implementación) llega como
`IstmoError::PluginError { bytes }`:

```rust
match hasher.dimensions(pixels).await {
    Ok(dims) => { /* ... */ }
    Err(IstmoError::PluginError { bytes }) => {
        let (err, _) = istmo::codec::decode::<HashError>(&bytes)?;
        log::warn!("{err}");
    }
    Err(other) => return Err(other.into()),
}
```

## Opcionalmente, llámalo desde Kotlin o Swift

Si el lado nativo necesita invocar el plugin, actívalo con un override
del lado de la app:

```toml
# istmo.toml (en la raíz del crate de la app)
[[app.plugin]]
id   = "acme.image_hasher"
role = "client"
```

El siguiente `cargo build` genera `ImageHasherClient.kt`,
`ImageHasherCodecs.kt`, `ImageHasherCodecsImpl.kt` e
`ImageHasherTypes.kt` en el paquete Kotlin de la app, y
`ImageHasherClient.swift` con sus codecs y tipos bajo
`ios/<App>/Plugins/ImageHasher/Generated/`. Los métodos conservan el
nombre de Rust:

```kotlin
val hasher = ImageHasherClient(ImageHasherCodecsImpl())
val phash: ULong = hasher.perceptual_hash(bytes)
```

```swift
let hasher = ImageHasherClient(codecs: ImageHasherCodecsImpl())
let phash = try await hasher.perceptual_hash(bytes: data)
```

Los errores de dominio se lanzan como `ImageHasherException`, con el
payload crudo y el `HashError` decodificado cuando decodifica.

## Testing

Los plugins Rust-hosted se testean como Rust async plano; cualquier
executor maneja los futures:

```rust
#[test]
fn hashes_a_png() {
    let png = include_bytes!("../tests/sample.png").to_vec();
    let out = pollster::block_on(ImageHasherImpl.perceptual_hash(png)).unwrap();
    assert_ne!(out, 0);
}
```

Para tests de integración, registra el host en un runtime mock y llama
al cliente igual que lo haría tu app:

```rust
let rt = istmo::Runtime::mock()
    .host(ImageHasherHost::new(ImageHasherImpl))
    .runtime;
let hasher = ImageHasherClient::from_runtime(&rt)?;
let phash = pollster::block_on(hasher.perceptual_hash(png))?;
```

## Siguiente

- Agrega un stream: [Streams](/istmo/es/writing-plugins/streams/).
- Cancelación cooperativa: [Cancelación](/istmo/es/writing-plugins/cancellation/).
- Si realmente quieres hablar con Kotlin/Swift: [Plugin native-hosted](/istmo/es/writing-plugins/native-hosted/).
