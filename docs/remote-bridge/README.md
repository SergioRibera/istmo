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
3. App process installs `RemotePluginRouter` with the set of plugin ids
   the `:remote` cdylib hosts.
4. App-process `IstmoRuntime.onCall` consults `RemotePluginRouter`
   before local dispatch — matching ids are forwarded, misses fall
   through to `handlers[pluginId]`.
5. `:remote`-side `IstmoRuntime` receives envelopes via
   `nativeInjectEnvelope`; local hosts fire; responses hop back via the
   reverse bridge.

## Still missing (follow-ups)

- `IstmoRuntime.nativeInjectEnvelope(bytes)` JNI export in
  `istmo-android`.
- `IstmoRuntime.nativeEncodeCall(...)` companion so Kotlin can
  serialise the `onCall` args back into an Envelope. Alternatively add
  an outbound "envelope bytes" callback on the pump that hands raw
  bytes instead of typed args.
- `istmo::runtime!` macro `remote: [...]` section that populates the
  Rust-side declared set + emits a Kotlin-side helper listing the ids
  for `RemotePluginRouter.install(...)`.
