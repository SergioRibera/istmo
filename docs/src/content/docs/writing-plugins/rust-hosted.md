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

Note: `#[istmo::plugin]` desugars every `async fn` in the trait to
`impl Future + Send + '_`, so you implement the trait with plain
`async fn` on Rust 1.75+. No `#[async_trait]` needed.

## Wire the host into the runtime

```rust
use image_hasher::{ImageHasherHost, ImageHasherImpl};

istmo::runtime!(
    hosts: [ImageHasherHost::new(ImageHasherImpl)],
);
```

`<T>Host` is emitted by the plugin macro. `<T>Host::new(impl)`
constructs the dispatcher and registers it with the runtime on init.

## Call it from Rust

Anywhere you have an `Arc<Runtime>`:

```rust
use image_hasher::ImageHasherClient;

let hasher = ImageHasherClient::from_runtime(&runtime)?;
let phash = hasher.perceptual_hash(pixels).await?;
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

The next `cargo build` regenerates `<T>Client.kt` / `<T>Client.swift`
files. Kotlin usage:

```kotlin
val hasher = ImageHasherClient(ImageHasherCodecsImpl())
val phash = hasher.perceptualHash(bytes)
```

## Testing

Rust-hosted plugins are trivially unit-testable because everything is
just async Rust:

```rust
#[tokio::test]
async fn hashes_a_png() {
    let png = include_bytes!("../tests/sample.png").to_vec();
    let out = ImageHasherImpl.perceptual_hash(png).await.unwrap();
    assert_ne!(out, 0);
}
```

For integration tests, spin up a runtime with `Runtime::new_isolated()`
and call the client just as your app would.

## Next

- Add a stream: [Streams](/istmo/writing-plugins/streams/).
- Cooperative cancel: [Cancellation](/istmo/writing-plugins/cancellation/).
- If you actually want to talk to Kotlin/Swift: [Native-hosted plugin](/istmo/writing-plugins/native-hosted/).
