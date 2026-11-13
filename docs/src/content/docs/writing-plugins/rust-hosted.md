---
title: Rust-hosted plugin
description: Ship the trait implementation in Rust; Kotlin and Swift optionally call in.
sidebar:
  order: 1
---

A Rust-hosted plugin puts the trait's real work in Rust. The native
side either does not touch the plugin at all (Rust code calls it) or
holds a generated `<T>Client` and calls in when it needs a result.

## When to reach for it

- Pure computation — hashing, parsing, cryptography, image processing.
- Business logic you want byte-identical across every platform.
- Code you already have in Rust that would rather stay put.

If your plugin needs to talk to a native SDK, request a permission, or
touch the platform's main thread, you probably want a
[native-hosted plugin](/istmo/writing-plugins/native-hosted/) instead.

:::note[Facade or core crates]
The snippets on these pages use the `istmo` facade, as code inside an
app crate does. A standalone plugin crate depends on `istmo-core` +
`istmo-macros` instead (the facade picks the Android activity flavour,
which is the app's choice) and passes `crate = "::istmo_core"` to
`#[plugin]` and `wincode = "::wincode"` to `#[message]` — see
[Your first plugin](/istmo/getting-started/first-plugin/).
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

The error type of a `Result` return must implement
`std::error::Error` (hence the `Debug` derive); the macro asserts it.

`istmo.toml`:

```toml
[plugin]
id          = "acme.image_hasher"
client_type = "::image_hasher::ImageHasherClient"
# Nothing native implements this plugin: keep it out of the apps'
# generated IstmoPluginRegistry, which would otherwise construct an
# `ImageHasherBackendImpl` on Android and iOS.
auto_register = false
```

`build.rs`:

```rust
fn main() { istmo_build::emit(); }
```

## Implement the trait

Right next to the trait definition:

```rust
pub struct ImageHasherImpl;

impl ImageHasher for ImageHasherImpl {
    async fn perceptual_hash(&self, bytes: Vec<u8>) -> Result<u64, HashError> {
        // your real hashing logic
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

`#[istmo::plugin]` desugars every `async fn` in the trait to
`impl Future + Send + '_`, so you implement it with plain `async fn`.
No `#[async_trait]` needed.

## Wire the host into the runtime

In the app crate:

```rust
istmo::runtime!(
    hosts: [image_hasher::ImageHasher => image_hasher::ImageHasherImpl],
);
```

Each `hosts:` entry is `Trait => expression`: the macro registers
`<Trait>Host::new(expression)`, so `image_hasher::ImageHasherHost` must
resolve. The client comes from the plugin's `client_type` through
[auto-wiring](/istmo/advanced/auto-wiring/). A plugin declared inside
the app crate has no manifest, so list its client as well —
`plugins: [ImageHasherClient]` — or calls fail with
`IstmoError::PluginNotDeclared`.

## Call it from Rust

Anywhere you have an `Arc<Runtime>`:

```rust
use image_hasher::ImageHasherClient;

let hasher = ImageHasherClient::from_runtime(&runtime)?;
let phash = hasher.perceptual_hash(pixels).await?;
```

Every client method returns `Result<T, IstmoError>`. A domain error
(`Err(HashError)` from the implementation) arrives as
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

## Optionally, call it from Kotlin or Swift

If the native side needs to invoke the plugin, opt in with an app-side
override:

```toml
# istmo.toml (at the app crate root)
[[app.plugin]]
id   = "acme.image_hasher"
role = "client"
```

The next `cargo build` generates `ImageHasherClient.kt`,
`ImageHasherCodecs.kt`, `ImageHasherCodecsImpl.kt` and
`ImageHasherTypes.kt` in the app's Kotlin package, and
`ImageHasherClient.swift` with its codecs and types under
`ios/<App>/Plugins/ImageHasher/Generated/`. Method names keep the Rust
spelling:

```kotlin
val hasher = ImageHasherClient(ImageHasherCodecsImpl())
val phash: ULong = hasher.perceptual_hash(bytes)
```

```swift
let hasher = ImageHasherClient(codecs: ImageHasherCodecsImpl())
let phash = try await hasher.perceptual_hash(bytes: data)
```

Domain errors are thrown as `ImageHasherException`, carrying the raw
payload and the decoded `HashError` when it decodes.

## Testing

Rust-hosted plugins are unit-testable as plain async Rust; any executor
drives the futures:

```rust
#[test]
fn hashes_a_png() {
    let png = include_bytes!("../tests/sample.png").to_vec();
    let out = pollster::block_on(ImageHasherImpl.perceptual_hash(png)).unwrap();
    assert_ne!(out, 0);
}
```

For integration tests, register the host on a mock runtime and call the
client just as your app would:

```rust
let rt = istmo::Runtime::mock()
    .host(ImageHasherHost::new(ImageHasherImpl))
    .runtime;
let hasher = ImageHasherClient::from_runtime(&rt)?;
let phash = pollster::block_on(hasher.perceptual_hash(png))?;
```

## Next

- Add a stream: [Streams](/istmo/writing-plugins/streams/).
- Cooperative cancel: [Cancellation](/istmo/writing-plugins/cancellation/).
- If you actually want to talk to Kotlin/Swift: [Native-hosted plugin](/istmo/writing-plugins/native-hosted/).
