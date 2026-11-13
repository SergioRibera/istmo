---
title: Architecture
description: How Istmo wires a Rust cdylib to Kotlin and Swift with a single binary protocol.
sidebar:
  order: 1
---

An Istmo app has three actors:

1. **Your Rust code.** Business logic, state, UI (if you ship a Rust UI
   framework like `eframe`), plugin traits.
2. **The `Runtime`.** A single process-wide object owned by
   `istmo-core`. It routes calls, manages plugin lifetimes, and
   serializes envelopes on and off the wire.
3. **The native side.** Kotlin on Android, Swift on iOS. Only present
   where the platform demands it — permissions, native SDKs, background
   execution.

Everything crosses between (1) + (2) and (3) through **one binary
protocol** — a wincode-encoded `Envelope` carrying a `Frame` variant.
No per-plugin trampolines, no JSON, no reflection.

## The Runtime

You declare the runtime once, at module level in the app crate, with
the `istmo::runtime!` macro:

```rust
istmo::runtime!(
    plugins: [SignInClient, BatteryClient],
    remote: [PushNotifications],
    hosts: [ImageHasher => ImageHasherImpl],
    services: [SyncService => Syncer],
    workers: [Backup => NightlyBackup],
);
```

Every section is optional:

- `plugins:` — clients you'll call from Rust (paths to `<Trait>Client`
  types).
- `remote:` — plugins whose backend lives in a **separate OS process**
  (Android `:remote` pattern). Each entry is the plugin trait or its
  `<Trait>Client`.
- `hosts:` — Rust implementations of plugin traits, written
  `Trait => expr` and registered as `<Trait>Host::new(expr)`.
- `services:` / `workers:` — `#[istmo::service]` / `#[istmo::worker]`
  implementations, registered as `<Trait>Adapter::new(expr)` /
  `<Trait>WorkerAdapter::new(expr)`.

The macro expands to `__istmo_configure_runtime(init: RuntimeInit) ->
RuntimeInit`, which the Android and iOS transports call when the
platform starts the runtime. Plugins that `istmo_build::emit()` finds in
your dependencies are added on top
([Auto-wiring](/istmo/advanced/auto-wiring/)). A desktop binary builds
its runtime itself with `Runtime::init(RuntimeConfig::inline())` and
registers its hosts on the returned runtime.

From there, a plugin client binds to a runtime with
`SignInClient::from_runtime(&runtime)`, or to the process-global one with
`SignInClient::acquire()`.

## The Frame protocol

A `Frame` is the unit of everything: calls, responses, events, cancels,
handle releases. It is defined in `istmo-core` and serialized with
wincode in bincode 2's `standard()` layout. The current protocol
version is **4** (see [Frame protocol](/istmo/concepts/frame-protocol/)
for the history).

| Variant               | Sent by           | Meaning                                       |
| --------------------- | ----------------- | --------------------------------------------- |
| `Call`                | caller            | Invoke a plugin method                        |
| `Respond`             | host              | Result of a `Call`                            |
| `Event`               | host              | One stream item                               |
| `StreamEnd`           | host              | Terminator for a stream                       |
| `Cancel`              | caller            | Cooperative cancel of a call or stream        |
| `CreateInstance`      | caller            | Build a stateful plugin instance              |
| `DestroyInstance`     | caller            | Release a stateful plugin instance            |
| `EarlyEvent`          | native side       | State published before Rust asked for it      |
| `ReleaseNativeHandle` | Rust              | A native-owned object should be freed         |
| `Notify`              | caller            | Fire-and-forget one-way call                  |

Caller and host can sit on either side: Rust calls Kotlin/Swift for a
native-hosted plugin, Kotlin/Swift call Rust for a Rust-hosted one.

Neither Kotlin nor Swift encodes an `Envelope`. Typed FFI wrappers on
each side build the frame Rust-side, so the JNI and FFI boundaries only
ever carry the inner payload bytes.

## Where plugins live

Every plugin is a **Rust crate** that:

- Declares the trait shape with `#[istmo::plugin]`.
- Publishes a `Contract` describing that shape at build time (via
  `istmo_build::emit()`).
- Optionally ships native backends under `native/android/` and
  `native/ios/`, which the app compiles in.

The app's build regenerates the Kotlin/Swift dispatchers, types and
codecs for every contract on each compile.

Two hosting models exist:

- [**Rust-hosted**](/istmo/concepts/native-vs-rust-hosted/#rust-hosted) — Rust
  implements the trait. Kotlin/Swift call *into* Rust (typical for
  pure-computation plugins).
- [**Native-hosted**](/istmo/concepts/native-vs-rust-hosted/#native-hosted) —
  Kotlin/Swift implement the trait. Rust calls *out* (typical for
  platform-integration plugins).

Both look identical from Rust — you always hold a `<T>Client`. The only
difference is where the code that services the call physically runs.

## Cross-crate metadata handover

An Istmo plugin does not import a schema from its consumer or vice
versa. Contracts and native dependencies travel through Cargo's build
script `DEP_<links>_*` env var mechanism:

1. Plugin `build.rs` calls `istmo_build::emit()`.
2. That emits `DEP_<links>_ISTMO_CONTRACTS`, `DEP_<links>_ISTMO_MANIFEST`
   and, when declared, `DEP_<links>_ISTMO_NATIVE_DEPS` (Gradle + SwiftPM
   coordinates), `DEP_<links>_ISTMO_ASSETS` and
   `DEP_<links>_ISTMO_NATIVE_{ANDROID,IOS,MACOS}`.
3. App `build.rs` calls `istmo_build::emit()`.
4. That walks every `DEP_*_ISTMO_CONTRACTS`, generates Kotlin and Swift
   dispatchers for each, and writes them into `android/` and `ios/`.

Generated files are only rewritten when their contents change, so a
rebuild with the same inputs leaves them untouched.

## Threading & executors

Istmo is **executor-agnostic**. It uses `flume` channels and
`pollster::block_on` internally, so:

- Your app can pair it with `tokio`, `smol`, `async-std`, `pollster`, or
  a hand-rolled loop. Nothing in `istmo-core` locks you in.
- Each unary `<T>Client` method is an `async fn`. Stream methods return a
  `TypedStream`, which you drive with `recv()` (blocking),
  `recv_async()` or `try_recv()` from any thread or executor.
- Each mobile transport runs one dedicated OS thread — a JNI-attached
  daemon on Android (`istmo-android-pump`), `istmo-ios-pump` on iOS —
  that delivers outbound frames to the Kotlin/Swift callbacks.

## Next

- Zoom in on the split: [Native vs. Rust-hosted](/istmo/concepts/native-vs-rust-hosted/).
- Understand contracts: [Contracts](/istmo/concepts/contracts/).
- See the wire types: [Frame protocol](/istmo/concepts/frame-protocol/).
