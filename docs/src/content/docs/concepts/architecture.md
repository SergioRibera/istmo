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
protocol** — a length-prefixed `bincode`-encoded `Envelope` carrying a
`Frame` variant. No per-plugin trampolines, no JSON, no reflection.

## The Runtime

You instantiate the runtime once with the `istmo::runtime!` macro:

```rust
istmo::runtime!(
    plugins: [SignInClient, BatteryClient],
    services: [SyncService],
    remote: [PushNotificationsClient],
);
```

The macro reads three optional sections:

- `plugins:` — clients you'll call directly from Rust.
- `services:` — long-running services the runtime should own.
- `remote:` — plugins whose backend lives in a **separate OS process**
  (Android `:remote` service pattern).

At compile time it produces `Runtime::new()` returning an
`Arc<Runtime>`. From there, every plugin client acquires the runtime
with `SignInClient::from_runtime(&runtime)`.

## The Frame protocol

A `Frame` is the unit of everything: calls, responses, events, cancels,
handle releases. It is defined in `istmo-core` and serialized with
`bincode 2`. The current protocol version is **4** (see [Frame
protocol](/istmo/concepts/frame-protocol/) for the history).

Variants you'll encounter:

| Variant                | Direction        | Meaning                                   |
| ---------------------- | ---------------- | ----------------------------------------- |
| `Call`                 | Rust → native    | Invoke a plugin method                    |
| `Respond`              | native → Rust    | Result of a Call                          |
| `Event`                | native → Rust    | One stream item                           |
| `StreamEnd`            | native → Rust    | Terminator for a stream                   |
| `Cancel`               | Rust → native    | Cooperative cancel of a Call or stream    |
| `EarlyEvent`           | native → Rust    | State published before Rust asked for it  |
| `ReleaseNativeHandle`  | Rust → native    | Native-owned object should be freed       |
| `Notify`               | Rust → native    | Fire-and-forget one-way call              |

Rust never encodes an `Envelope` by hand and neither does Kotlin/Swift.
Typed FFI wrappers on each side build the frame Rust-side, so the JNI
and FFI boundaries only ever carry the inner payload bytes.

## Where plugins live

Every plugin is a **Rust crate** that:

- Declares the trait shape with `#[istmo::plugin]`.
- Publishes a `Contract` describing that shape at build time (via
  `istmo_build::emit()`).
- Optionally ships Kotlin/Swift dispatcher templates that the app
  regenerates on every compile.

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
2. That emits `DEP_<plugin>_ISTMO_CONTRACT` and
   `DEP_<plugin>_NATIVE_DEPS` (Gradle + SwiftPM coordinates).
3. App `build.rs` calls `istmo_build::emit()`.
4. That walks every `DEP_*_ISTMO_CONTRACT`, generates Kotlin and Swift
   dispatchers for each, and writes them into `android/` and `ios/`.

Everything is deterministic and repeatable — regenerating produces the
same bytes.

## Threading & executors

Istmo is **executor-agnostic**. It uses `flume` channels and
`pollster::block_on` internally, so:

- Your app can pair it with `tokio`, `smol`, `async-std`, `pollster`, or
  a hand-rolled loop. Nothing in `istmo-core` locks you in.
- Each `<T>Client` method returns `impl Future`. Streams return
  `impl Stream + Unpin` you can drive from anywhere.
- The native side gets its own dedicated OS thread per platform — a JNI
  daemon on Android, a `Thread` on iOS — that pumps outbound frames
  onto the platform runloop.

## Next

- Zoom in on the split: [Native vs. Rust-hosted](/istmo/concepts/native-vs-rust-hosted/).
- Understand contracts: [Contracts](/istmo/concepts/contracts/).
- See the wire types: [Frame protocol](/istmo/concepts/frame-protocol/).
