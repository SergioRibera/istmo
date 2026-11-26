---
title: Frame protocol
description: The single wire format that flows across JNI and FFI.
sidebar:
  order: 5
---

Everything that crosses between Rust and the native side of an Istmo
app is a **frame** wrapped in an **envelope**. There is only one format,
one codec (wincode, laid out like bincode 2's `standard()`: varint
integers, little endian), and one protocol version. Neither Kotlin nor
Swift encodes envelopes — they hand Rust typed payloads and the runtime
does the packing.

You do not need to read this page to write a plugin. Keep it as
reference if you are debugging a wire issue or writing a transport
backend.

## Envelope

```rust
pub struct Envelope {
    pub version: u16,   // PROTOCOL_VERSION, currently 4
    pub frame: Frame,
}
```

`Envelope::from_wire_bytes` rejects an envelope whose `version` differs
from the receiver's `PROTOCOL_VERSION` with
`IstmoError::ProtocolVersionMismatch`, so two builds of different
protocol versions never misread each other.

## Frame variants

| Variant                                              | Sent by     | Purpose                                                    |
| ---------------------------------------------------- | ----------- | ---------------------------------------------------------- |
| `Call { call_id, plugin_id, instance_id, method, payload }` | caller | Invoke a plugin method with a serialized arg tuple   |
| `Respond { call_id, result }`                        | host        | `Ok(payload)` or `Err(payload)` for a `Call`               |
| `Cancel { call_id }`                                 | caller      | Cooperative cancel of a call or its stream                 |
| `Event { stream_id, payload }`                       | host        | One item on a stream                                       |
| `StreamEnd { stream_id, reason }`                    | host        | Ends a stream: `Complete`, `Cancelled` or `Error(payload)` |
| `CreateInstance { call_id, plugin_id, payload }`     | caller      | Construct a stateful plugin instance from its config       |
| `DestroyInstance { instance_id }`                    | caller      | Release a stateful plugin instance                         |
| `EarlyEvent { channel, kind, payload }`              | native side | State published before Rust subscribed                     |
| `ReleaseNativeHandle { handle_id }`                  | Rust        | Release a native-owned resource id                         |
| `Notify { plugin_id, instance_id, method, payload }` | caller      | Fire-and-forget one-way call, no response expected         |

Caller and host can be on either side: Rust is the caller for a
native-hosted plugin, Kotlin/Swift for a Rust-hosted one. A stream
reuses the call's id as its `stream_id`.

`EarlyEvent` carries a retention policy, `kind`: `Latest` keeps only
the newest value, `Queue { capacity }` keeps a bounded FIFO that drains
into the first subscriber.

## Identifiers

- `call_id`, `stream_id` and `instance_id` are `u64` newtypes allocated
  by the side that starts the call.
- `handle_id` (`NativeHandleId`) is allocated by whoever owns the
  object: the Kotlin and Swift runtimes count up from `1`, Rust-side
  hosts from `1 << 63` (`Runtime::alloc_native_handle_id`), so the two
  ranges never collide.
- `plugin_id` is the plugin's id: the `name` of its
  `#[istmo::plugin(name = "…")]` trait, which its `istmo.toml` repeats
  as `[plugin] id`.
- `channel` for `EarlyEvent` is a string owned by the plugin (typically
  the plugin id plus a sub-channel name).

## Version history

| Version | Change                                                               |
| ------- | -------------------------------------------------------------------- |
| 1       | Initial frame set                                                    |
| 2       | Added `EarlyEvent { channel, kind, payload }`                        |
| 3       | Added `ReleaseNativeHandle { handle_id }`                            |
| 4       | Added `Notify { plugin_id, instance_id, method, payload }`           |

`PROTOCOL_VERSION` is bumped whenever a `Frame` variant is added or one
changes shape. Because the version must match exactly, the Rust library
and the Kotlin/Swift runtime of one app always come from the same
release.

## Where the boundary is

- **JNI (Android):** inbound frames enter Rust through typed `external`
  functions on `dev.istmo.runtime.IstmoRuntime` — `nativeSubmitCall`,
  `nativeSubmitResponse`, `nativeSubmitEvent`, `nativeSubmitStreamEnd`,
  `nativeSubmitCancel`, `nativeSubmitNotify`, `nativeSubmitEarlyLatest`,
  `nativeSubmitEarlyQueue`. Outbound frames reach Kotlin through static
  callbacks on the same object: `onCall`, `onRespond`, `onEvent`,
  `onStreamEnd`, `onCancel`, `onNotify`, `onCreateInstance`,
  `onDestroyInstance`, `onReleaseNativeHandle`.
- **FFI (iOS):** the C functions declared in `istmo_ffi.h`
  (`istmo_ios_submit_call`, `istmo_ios_submit_response`,
  `istmo_ios_submit_notify`, …) take the frame's fields as arguments,
  and outbound frames arrive through the `IstmoIosCallbacks` table
  passed to `istmo_ios_start`.

Some entries skip a frame where they can, without changing what the
frames mean:

- **Answers in place.** `nativeCall` (Android) and `istmo_ios_call`
  (iOS) carry a `Call` and return its answer when an inline Rust host
  gave it before returning (`nativeCall` returns the bytes or throws
  `PluginException`; `istmo_ios_call` returns `ISTMO_IOS_CALL_OK` /
  `_ERR` with the bytes in an `IstmoIosReply` the caller frees with
  `istmo_ios_free_reply`). Otherwise the answer is a `Respond` frame as
  usual, and it always is while a frame queued before it is still on its
  way. `istmo_ios_call` also answers `_ERR` for a call it could not
  submit.
- **Rust → Kotlin without the pump.** While nothing waits in the
  outbound channel, a `Call` from Rust is handed to Kotlin's
  `onCallDirect` on the calling thread (its return value is the answer,
  or `null` for a later `Respond`), and a stream item to `onEvent`.
  Consecutive items of one stream that did queue cross in one
  `onEvents(streamId, Array<ByteArray>)`.
- **Credit streams.** `nativeSubmitStream` opens a stream whose items
  Rust sends only against credit (a window, then `nativeStreamCredit`
  grants as the Kotlin collector takes items). Swift streams have no
  credit yet.
- **Items that cannot wait.** `nativeSubmitEvent` returns `false`, and
  delivers nothing, when the consumer's buffer is full and the calling
  thread must never block (the pump, Rust's async workers). A Kotlin
  stream relaying items that arrive on such a thread submits that item
  again from `Dispatchers.IO`.

Every argument is a plain id, string or payload byte buffer. Envelope
wrapping, version stamping and frame serialization happen Rust-side.

## Debugging

`Envelope::to_wire_bytes` / `Envelope::from_wire_bytes` are public in
`istmo-core`, so you can round-trip a frame in a unit test to check a
change. `Runtime::dispatch_inbound` and `Runtime::inject_wire_envelope`
feed a decoded or raw envelope straight into a runtime, and the
`outbound` receiver returned by `Runtime::init` shows every frame it
sends.

## Next

- See how Rust services stream: [Streams](/istmo/writing-plugins/streams/).
- See how native handles roundtrip: [Native handles](/istmo/writing-plugins/native-handles/).
- Cross-process bridging: [Remote process](/istmo/advanced/remote-process/).
