# `:remote` process bridge — reference material

Files in this directory are **not** built by cargo. Copy them into your
Android app when you want a plugin trait to live in a `:remote` process.

## Files

- `IIstmoBridge.aidl` — AIDL wire shape. Drop into
  `src/main/aidl/dev/istmo/remote/`.
- `IstmoRemoteBridge.kt` — hand-drafted `Service` + client stub +
  routing helper. Drop into `src/main/java/dev/istmo/remote/`.

## Rust-side primitives (in-tree, tested)

- `Envelope::to_wire_bytes()` — bincode-encode an envelope for
  transport.
- `Envelope::from_wire_bytes(bytes)` — decode + verify
  `PROTOCOL_VERSION`.
- `Runtime::inject_wire_envelope(bytes)` — receive-side one-liner.

End-to-end coverage: `crates/core/tests/remote_bridge.rs` proves the
pattern with two runtimes in a single process shuttling bytes through
flume channels that stand in for AIDL.

## Wiring order

1. Manifest: declare the target Service with
   `android:process=":remote"` on the remote side and default process on
   the app side. Both point to `IstmoRemoteBridgeService`.
2. On startup, each side creates an `IstmoRemoteBridgeClient` targeted
   at the OTHER process's Service class name and calls `bind()`.
3. Rust-side (both processes): call
   `runtime.declare_remote_plugin(pluginId)` for every trait that lives
   in the peer process. The classifier now steers matching outbound
   frames to the sink instead of the typed pump.
4. Kotlin-side: assign the bridge to
   `IstmoRuntime.remoteEnvelopeSink = { bytes -> bridge.submit(bytes) }`
   (or call `installRemoteBridge(bridge)`).
5. `:remote`-side `IstmoRuntime` receives envelopes via
   `nativeInjectEnvelope`; local hosts fire; responses hop back via the
   reverse bridge.

## Still missing (follow-ups)

- `istmo::runtime!` macro `remote: [...]` section that populates the
  Rust-side declared set + emits a Kotlin-side helper listing the ids
  (today the plugin author calls `runtime.declare_remote_plugin(id)`
  manually — the macro sugar is the last piece).
