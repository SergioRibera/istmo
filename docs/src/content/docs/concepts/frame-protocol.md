---
title: Frame protocol
description: The single bincode-encoded wire format that flows across JNI and FFI.
sidebar:
  order: 5
---

Everything that crosses between Rust and the native side of an Istmo
app is a **frame** wrapped in an **envelope**. There is only one format,
one codec (`bincode 2`), and one version byte. Rust and Kotlin/Swift
never encode envelopes by hand — they hand each other typed payloads and
the runtime does the packing.

You do not need to read this page to write a plugin. Keep it as
reference if you are debugging a wire issue or writing a transport
backend.

## Envelope

```rust
pub struct Envelope {
    pub version: u8,        // PROTOCOL_VERSION, currently 4
    pub frame: Frame,
}
```

The version byte lets receivers reject envelopes from a mismatched
build. Every runtime binary knows the versions it can decode.

## Frame variants

| Variant                   | Direction        | Purpose                                                         |
| ------------------------- | ---------------- | --------------------------------------------------------------- |
| `Call { call_id, ... }`   | Rust → native    | Invoke a plugin method with a serialized arg tuple              |
| `Respond { call_id, ... }`| native → Rust    | Return an `Ok(payload)` or `Err(payload)` for a Call            |
| `Event { stream_id, ... }`| native → Rust    | One item on a stream                                            |
| `StreamEnd { stream_id, kind }`| native → Rust | Terminates a stream (`Complete`, `Cancelled`)                 |
| `Cancel { call_id }`      | Rust → native    | Cooperative cancel of a Call or its downstream stream           |
| `EarlyEvent { channel, kind, payload }` | native → Rust | State published before Rust subscribed                    |
| `ReleaseNativeHandle { handle_id }` | Rust → native | Release a native-owned resource ID                       |
| `Notify { plugin_id, ... }` | Rust → native  | Fire-and-forget one-way call, no response expected              |
| `CreateInstance / DestroyInstance` | Rust → native | Stateful plugins that require init-side config              |

## Identifiers

- `call_id`, `stream_id`, `instance_id`, `handle_id` are all
  monotonically increasing `u64`s allocated on the runtime.
- `plugin_id` is the string you declared in `istmo.toml`.
- `channel` for `EarlyEvent` is a domain-specific string owned by the
  plugin (typically the plugin's `id` plus a sub-channel name).

## Version history

| Version | Change                                                               |
| ------- | -------------------------------------------------------------------- |
| 1       | Initial: Call/Respond/Event/StreamEnd/Cancel                         |
| 2       | Added `EarlyEvent { channel, kind, payload }`                        |
| 3       | Added `ReleaseNativeHandle { handle_id }`                            |
| 4       | Added `Notify { plugin_id, instance_id, method, payload }`           |

The protocol is additive-only. Older payloads decode on newer runtimes,
but newer variants are rejected on older ones. Bump `PROTOCOL_VERSION`
in `istmo-core` whenever you extend the enum.

## Where the boundary is

- **JNI (Android):** `IstmoRuntime.onCall(payload: ByteArray)` /
  `nativeSubmitCall(bytes)`. Only the inner payload bytes cross —
  Kotlin never encodes an `Envelope`.
- **FFI (iOS):** `istmo_ios_submit_call(payload, len)` and a callback
  table registered at `istmo_ios_start` — same rule, payload-only.

Everything else — envelope wrapping, version stamping, frame
serialization — happens Rust-side. A typed wrapper on each platform
(`nativeSubmitEarlyLatest`, `istmo_ios_submit_notify`) exposes the
variants your app actually uses.

## Debugging

Enable `RUST_LOG=istmo_core=trace` to see every frame the runtime emits
and receives. The `Envelope::to_wire_bytes` / `Envelope::from_wire_bytes`
functions are public in `istmo-core`, so you can round-trip a frame in a
unit test to verify a change.

## Next

- See how Rust services stream: [Streams](/writing-plugins/streams/).
- See how native handles roundtrip: [Native handles](/writing-plugins/native-handles/).
- Cross-process bridging: [Remote process](/advanced/remote-process/).
