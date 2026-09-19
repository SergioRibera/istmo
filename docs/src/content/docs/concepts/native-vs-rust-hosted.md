---
title: Native vs. Rust-hosted
description: When to implement the plugin trait in Rust and when to implement it in Kotlin or Swift.
sidebar:
  order: 3
---

Every Istmo plugin trait has one implementation. The choice of **where**
that implementation lives — Rust or the native platform — decides
whether your plugin is Rust-hosted or native-hosted.

Both look identical from a caller's point of view: you hold a
`<T>Client`, you call its methods, you await the result. The
distinction only matters when you write the plugin.

## Native-hosted

**The trait is implemented in Kotlin and Swift.** Rust only ever calls
out.

Choose this when the work must happen on the platform's own runtime:

- Requesting permissions.
- Displaying system UI (dialogs, share sheets, sign-in flows).
- Reading device sensors, battery, orientation.
- Talking to a native SDK you cannot rebuild in Rust (Google Play
  Services, Live Activities, HealthKit).

The Rust plugin crate contributes:

- The trait definition (`#[istmo::plugin]`).
- The `Contract` (auto-derived).
- Optionally, reference native backends under
  `native/{android,ios}/`, published as templates the app copies once
  and edits.

The app contributes:

- A concrete `<T>BackendImpl` (Kotlin/Swift) that speaks to the
  platform.
- A registration line during `Activity.onCreate` / `App.init` to hook
  the backend into `IstmoRuntime`.

Reference plugins: [`google-sign-in`](/plugins/google-sign-in/),
[`data-store`](/plugins/data-store/),
[`live-activity`](/plugins/live-activity/).

## Rust-hosted

**The trait is implemented in Rust.** The Rust crate ships the real
logic; Kotlin/Swift optionally call *into* Rust.

Choose this when:

- The work is pure computation (image processing, cryptography,
  parsing).
- You want to share business logic across every platform, including
  desktop and headless CLI targets.
- You need the exact same behaviour byte-for-byte on Android and iOS.

The Rust plugin crate contributes:

- The trait definition **and** its concrete impl:
  ```rust
  #[istmo::plugin]
  pub trait ImageHasher {
      async fn perceptual_hash(&self, bytes: Vec<u8>) -> Result<u64, HashError>;
  }

  pub struct ImageHasherImpl;

  impl ImageHasher for ImageHasherImpl {
      async fn perceptual_hash(&self, bytes: Vec<u8>) -> Result<u64, HashError> {
          // real work here
      }
  }
  ```
- Registration inside `istmo::runtime! { hosts: [ImageHasherHost::new(ImageHasherImpl)] }`.

The app contributes:

- **Nothing on the native side, unless Kotlin/Swift want to call the
  plugin directly.** For that you enable the app-side
  `[[app.plugin]] role = "client"` override so Istmo generates a
  `<T>Client.kt` / `<T>Client.swift` calling stub.

## Decision matrix

|                       | Native-hosted                              | Rust-hosted                                |
| --------------------- | ------------------------------------------ | ------------------------------------------ |
| Real code lives in    | Kotlin / Swift                             | Rust                                       |
| Rust ships            | Trait, contract, docs, reference impl      | Trait, contract, real impl                 |
| Native ships          | Real impl (in your app)                    | Optional client stub                       |
| Typical use case      | Platform SDKs, permissions, sensors        | Business logic, algorithms, shared state   |
| Codegen `role`        | `host` (default)                           | `client` (opt-in per plugin)               |
| Threading             | Native runtime, `suspend fun`/`async`      | Rust `async` on any executor               |

## Which will you write?

For 90% of app-specific plugins you write native-hosted plugins that
wrap platform APIs, because that is where mobile apps spend their
integration budget. When you find yourself writing the same Kotlin and
Swift twice, that logic wants to move down into Rust and become a
Rust-hosted plugin.

## Next

- Native-hosted end-to-end: [Writing a native-hosted plugin](/writing-plugins/native-hosted/).
- Rust-hosted end-to-end: [Writing a Rust-hosted plugin](/writing-plugins/rust-hosted/).
