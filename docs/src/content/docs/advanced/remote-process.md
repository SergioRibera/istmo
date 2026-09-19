---
title: Remote process bridge
description: Run a plugin's backend in a separate OS process on Android.
sidebar:
  order: 1
---

The Android runtime supports the classic `:remote` process pattern —
where a plugin's backend runs in a **different OS process** from the
UI. Useful when:

- The plugin's SDK crashes are frequent and you want them to not take
  down the UI process.
- The plugin holds a large resident memory footprint (an ML model, a
  media pipeline) that survives your UI going through Trim states.
- You want independent process death for OOM protection.

## Wire shape

Each process runs its own `Runtime`. A **bridge** — an AIDL Binder
interface — shuttles `Envelope` bytes back and forth. Every outbound
frame from the UI process whose plugin id is declared "remote" is
serialized, sent over Binder, and injected into the `:remote` process's
runtime via `Runtime::inject_wire_envelope(bytes)`.

The Rust side does the classification; Kotlin only forwards bytes.

```
   UI process                     :remote process
   ┌──────────────┐  Envelope     ┌──────────────┐
   │  Runtime     │──── bytes ───►│  Runtime     │
   │              │◄── bytes ─────│              │
   │  send_out    │               │  inject_wire │
   │  → matches   │               │  → dispatch  │
   │   remote id? │               │              │
   └──────────────┘               └──────────────┘
        │  yes → RemoteEnvelopeSink
        ▼
     Kotlin Binder shuttle
```

## Declare which plugins are remote

Two levers:

1. **Plugin default** — the plugin's own `istmo.toml`:
   ```toml
   [plugin]
   default_deployment = "remote"
   ```
2. **App override** — even if the plugin defaults to `local`, the app
   can force it remote:
   ```toml
   # istmo.toml (at the app crate root)
   [[remote_override]]
   plugin     = "acme.large_ml_model"
   deployment = "remote"
   ```

`emit_wiring_env()` (invoked by `istmo_build::emit()`) reads these and
emits `ISTMO_AUTO_REMOTE` env vars the `istmo::runtime!` macro
consumes.

## Rust-side wiring

```rust
istmo::runtime!(
    plugins:  [SignInClient, DataStoreClient],
    remote:   [LargeModelClient],
);
```

The macro:

- Registers the local plugins as usual.
- Marks `LargeModelClient`'s plugin id as remote via
  `Runtime::declare_remote_plugin`.
- Installs a `RemoteEnvelopeSink` closure (`Arc<dyn Fn(Vec<u8>)>`) —
  the app must supply this at runtime with `install_remote_envelope_sink`,
  passing the Kotlin bridge as the destination.

You typically supply the sink like:

```rust
istmo_runtime.install_remote_envelope_sink(Arc::new(|bytes| {
    // Kotlin-side implementation forwards to Binder
    unsafe { native_forward_to_bridge(bytes.as_ptr(), bytes.len()) }
}));
```

## Kotlin-side bridge

Both processes' `IstmoRuntime` expose:

- **`onRemoteEnvelope(bytes: ByteArray)`** — called by JNI when Rust
  emits outbound bytes for the remote process.
- **`nativeInjectEnvelope(bytes: ByteArray)`** — called on the receive
  side to hand inbound bytes to the runtime.

Together with a two-way AIDL interface, they form the shuttle:

```aidl
// IIstmoBridge.aidl
interface IIstmoBridge {
    void submitEnvelope(in byte[] bytes);
}
```

Wire the `IBinder` in both processes, connect
`onRemoteEnvelope` → `remote.submitEnvelope`, and
`submitEnvelope` on receipt → `nativeInjectEnvelope`. The Rust side of
each runtime handles the rest.

## What crosses the bridge

Every frame variant is safe to cross:

- `Call` / `CreateInstance` / `Notify` — routed by plugin id + call id.
- `Respond` / `Cancel` — routed by call id (recorded via
  `mark_call_remote` on inbound Call).
- `Event` / `StreamEnd` — routed by stream id.
- `DestroyInstance` — routed by instance id.
- `EarlyEvent` / `ReleaseNativeHandle` — receive side short-circuits
  into the appropriate handler.

No per-frame-variant special-casing in Kotlin — it just forwards bytes.

## Testing

The Rust primitives are covered by unit tests without an emulator:

- `remote_bridge.rs` — spins up two `Runtime`s, wires an in-memory
  sink, verifies frames route correctly.
- `mark_call_remote_routes_hosted_respond_through_sink` — verifies
  responses go back the way they came.

Full end-to-end (an actual Android `:remote` service) lands with the
first shipping consumer; the primitives are stable.

## When not to reach for it

- Plugins that mostly bounce off system APIs (permissions, share
  sheet) — the overhead is real and there's no memory saving.
- Very frequent, low-latency plugins — every call now costs a Binder
  hop.
- Cross-platform plugins — the `:remote` shape is Android-only.
