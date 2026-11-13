---
title: Remote process bridge
description: Run a plugin's backend in a separate OS process on Android.
sidebar:
  order: 1
---

The runtime supports the classic Android `:remote` process pattern —
where a plugin's backend runs in a **different OS process** from the
UI. Useful when:

- The plugin's SDK crashes often and you do not want it to take down
  the UI process.
- The plugin holds a large resident memory footprint (an ML model, a
  media pipeline) you want to keep apart from the UI process.
- You want independent process death for OOM protection.

## Wire shape

Each process runs its own `Runtime`. A **bridge** — an AIDL Binder
interface — shuttles wire-encoded `Envelope` bytes back and forth. Every
outbound frame of a plugin declared remote is encoded, handed to the
runtime's `RemoteEnvelopeSink`, sent over Binder, and injected into the
other process's runtime with `Runtime::inject_wire_envelope(bytes)`.

The Rust side does the classification; Kotlin only forwards bytes.

```
   UI process                     :remote process
   ┌──────────────┐  Envelope     ┌──────────────┐
   │  Runtime     │──── bytes ───►│  Runtime     │
   │              │◄── bytes ─────│              │
   │  outbound    │               │  inject_wire │
   │  → remote    │               │  _envelope   │
   │   plugin?    │               │  → dispatch  │
   └──────────────┘               └──────────────┘
        │  yes → RemoteEnvelopeSink
        ▼
     Kotlin Binder shuttle
```

## Declare which plugins are remote

Two levers feed auto-wiring:

1. **Plugin default** — the plugin's own `istmo.toml`:
   ```toml
   [plugin]
   default_deployment = "remote"
   ```
2. **App override** — even if the plugin defaults to `local`, the app
   can force it remote (or back to local):
   ```toml
   # istmo.toml (at the app crate root)
   [[remote_override]]
   plugin     = "acme.large_model"
   deployment = "remote"
   ```

`istmo_build::emit()` turns these into the `ISTMO_AUTO_REMOTE` list the
`istmo::runtime!` macro consumes.

## Rust-side wiring

You can also list remote plugins by hand:

```rust
istmo::runtime!(
    plugins: [SignInClient, DataStoreClient],
    remote:  [LargeModel],   // the trait, or `LargeModelClient`
);
```

For each `remote:` entry the generated configuration calls
`RuntimeInit::remotes::<LargeModelClient>()`, i.e.
`Runtime::declare_remote_plugin("acme.large_model")`. From then on the
runtime steers that plugin's frames to the installed
`RemoteEnvelopeSink` (`Arc<dyn Fn(Vec<u8>) + Send + Sync>`) instead of
the regular transport.

On Android you do not install the sink yourself: `nativeStart` installs
one that hands the bytes to `IstmoRuntime.onRemoteEnvelope` on the pump
thread. Elsewhere (desktop, tests) install it with
`Runtime::install_remote_envelope_sink`:

```rust
use std::sync::Arc;

runtime.install_remote_envelope_sink(Arc::new(move |bytes: Vec<u8>| {
    // Hand `bytes` to your IPC channel; return quickly.
    let _ = to_peer.send(bytes);
}));
```

Without a sink, frames of remote plugins fall back to the outbound
channel.

## Kotlin-side bridge

`dev.istmo.runtime.IstmoRuntime` exposes:

- **`remoteEnvelopeSink: ((ByteArray) -> Unit)?`** — set it to the
  Binder proxy; `onRemoteEnvelope(bytes)` forwards every outbound remote
  envelope to it (and drops it while unset).
- **`nativeInjectEnvelope(bytes: ByteArray)`** — call it on the
  receiving side to hand inbound envelope bytes to the runtime.

Together with a two-way AIDL interface, they form the shuttle:

```aidl
// IIstmoBridge.aidl
package dev.istmo.remote;

interface IIstmoBridge {
    oneway void submitEnvelope(in byte[] envelope);
}
```

Both processes bind the other's service, set
`IstmoRuntime.remoteEnvelopeSink` to forward to
`remote.submitEnvelope`, and call `IstmoRuntime.nativeInjectEnvelope`
from their own `submitEnvelope`. Reference sources to copy — the AIDL
file and an `IstmoRemoteBridgeService` / `IstmoRemoteBridgeClient` pair
— live in the repository under `docs/remote-bridge/`. Each process must
also declare the plugins that live in the *other* process as remote.

## What crosses the bridge

The classifier routes every frame that belongs to a remote plugin:

- `Call` / `CreateInstance` / `Notify` — by plugin id.
- `Cancel` — by the call id of an outgoing remote call.
- `DestroyInstance` — by instance id (instances created through a remote
  `CreateInstance` are recorded automatically).
- On the hosting side, calls that arrive through
  `inject_wire_envelope` are tracked under their own origin, so their
  `Respond`, `Event` and `StreamEnd` frames go back through the sink —
  their ids may coincide with ids the local transport uses without
  clashing.

`EarlyEvent` and `ReleaseNativeHandle` are never routed to the sink;
they belong to the process's own native side. No per-frame-variant
special-casing in Kotlin — it just forwards bytes.

## Testing

The Rust primitives are covered by unit tests without an emulator, in
`crates/core/tests/remote_bridge.rs`:

- `app_to_remote_call_and_response_flow_over_bridge_bytes` — two
  `Runtime`s shuttling bytes through in-memory channels.
- `declare_remote_plugin_routes_matching_outbound_through_sink` —
  outbound frames of a remote plugin reach the sink.
- `mark_call_remote_routes_hosted_respond_through_sink` — responses go
  back the way they came.

There is no end-to-end example app with an actual Android `:remote`
service yet.

## When not to reach for it

- Plugins that mostly bounce off system APIs (permissions, share
  sheet) — the overhead is real and there's no memory saving.
- Very frequent, low-latency plugins — every call now costs a Binder
  hop.
- Cross-platform plugins — the `:remote` shape is Android-only.
