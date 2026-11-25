---
title: Frame sinks
description: Move video frames, audio buffers and other high-rate native buffers from a plugin to Rust without encoding or copying them.
sidebar:
  order: 5
---

A stream item travels the wire: it is encoded, routed and decoded, and
a Kotlin or Swift producer adds a JNI / FFI copy on top. That is fine
for events and samples, and far too much for a 4K frame sixty times a
second. A **frame sink** is the path for buffers that must never be
serialised: DMA-BUFs, `HardwareBuffer`s, `CVPixelBuffer`s / `IOSurface`s,
D3D11 textures, or plain CPU memory you already have.

The consumer opens a sink and passes its id to the plugin as an
ordinary method argument. The producer — a Rust host in the same
process, or a Kotlin / Swift backend — pushes **frames** into it: plugin
metadata plus a `NativeBuffer`. Nothing is encoded but the metadata (and
not even that from Rust), and dropping a frame releases its buffer to
the producer.

## Declaring a sink argument

```rust
use istmo::{FrameSinkId, plugin};

#[istmo::message]
#[derive(Debug, Clone)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    pub timestamp_ns: u64,
}

#[plugin(name = "acme.capture")]
pub trait Capture {
    /// Start capturing into `frames` until `stop`.
    async fn start(&self, frames: FrameSinkId<FrameInfo>) -> Result<(), CaptureError>;
    async fn stop(&self);
}
```

`FrameSinkId<M>` is a `u64` on the wire; `M` names the metadata each
frame carries, so producer and consumer agree on it at compile time. A
sink id is only valid as a method argument — a message field, return
value or container holding one is rejected by the macros' build-time
contract check.

## Consuming frames (Rust)

```rust
use istmo::SinkPolicy;

let sink = runtime.open_frame_sink::<FrameInfo>(SinkPolicy::latest())?;
capture.start(sink.id()).await?;

while let Some(frame) = sink.recv_async().await {
    let info: &FrameInfo = &frame.meta;
    if frame.dropped_before() > 0 {
        tracing::debug!(lost = frame.dropped_before(), "consumer fell behind");
    }
    if let Some(bytes) = frame.buffer.cpu_bytes() { /* CPU pixels */ }
    // `frame` drops here: the producer gets its buffer back.
}
```

`recv` (blocking), `recv_async` and `try_recv` return `None` once the
sink is closed and drained. Dropping the `FrameSink` closes it:
producers are told on their next push and queued frames are released.

### Backpressure

Producers never block. The policy decides what a full sink does, and
every lost frame is counted (`dropped_before()` on the next frame,
`stats()` for totals):

| Policy | Behaviour | Use for |
|---|---|---|
| `SinkPolicy::latest()` | depth 1, a new frame replaces the queued one | previews, mirroring |
| `SinkPolicy::queue(n)` | depth `n`, evict the oldest | encoders tolerating a hiccup |
| `SinkPolicy::queue(n).with_overflow(Overflow::DropNewest)` | depth `n`, refuse the new frame | consumers that need a contiguous prefix |

Hold frames only as long as you need them: pooled producers
(`ImageReader`, a compositor's buffer pool) reuse a buffer only after
its frame drops, so a consumer that keeps every frame stalls capture.

## Buffers

`NativeBuffer` owns one backing value and an optional release hook. The
backing's own `Drop` releases what it holds; hooks added with
`with_release` run after it. Cloning shares the buffer, so fanning a
frame out to several consumers copies nothing; release runs once, when
the last clone drops.

| `kind()` | Backing | Made by |
|---|---|---|
| `Cpu` | any `AsRef<[u8]>` (`Vec<u8>`, `Bytes`, a mapping) — `cpu_bytes()` | `NativeBuffer::from_cpu` |
| `DmaBuf` | `DmaBuf` (planes of `OwnedFd`, fourcc, modifier) — `dmabuf()` | `NativeBuffer::from_dmabuf` |
| `HardwareBuffer` | `istmo::android::HardwareBuffer` (`as_ptr()`, `desc()`) | Kotlin `IstmoFrameSink.submit(HardwareBuffer, …)` |
| `CvPixelBuffer` / `IoSurface` | `istmo::ios::RetainedBuffer` (`as_ptr()`) | Swift `IstmoFrameSink.submit(CVPixelBuffer, …)` |
| anything | your own type | `NativeBuffer::new(kind, value)` |

Reach a platform backing with `frame.buffer.downcast_ref::<T>()`, or
take it out with `into_backing::<T>()`, which returns a `BufferLease`
that keeps the producer's slot reserved until it drops.

## Producing frames from Rust

A Rust host resolves the id into a typed `FrameSender` — no bytes are
involved:

```rust
impl Capture for DesktopCapture {
    async fn start(&self, frames: FrameSinkId<FrameInfo>) -> Result<(), CaptureError> {
        // `self.runtime`: the `Arc<Runtime>` the host was built with.
        let sender = self
            .runtime
            .frame_sender(frames)
            .map_err(|_| CaptureError::SinkUnreachable)?;
        std::thread::spawn(move || {
            while let Some((pixels, info)) = next_frame() {
                let buffer = NativeBuffer::from_cpu(pixels);
                if let Err(err) = sender.push(info, buffer) {
                    if err.is_closed() { break; } // consumer gone
                }
            }
            // dropping the last `sender` clone ends the sink
        });
        Ok(())
    }
    /* ... */
}
```

Resolve the id once per session and clone the sender: each resolution
is its own producer, and the sink closes when the last clone of one
drops (or on `close()`).

## Producing frames from Kotlin and Swift

The generated backend receives an `IstmoFrameSink<FrameInfo>` instead of
an id:

```kotlin
override suspend fun start(frames: IstmoFrameSink<FrameInfo>) {
    reader.setOnImageAvailableListener({ r ->
        r.acquireLatestImage()?.use { image ->
            val status = frames.submit(image.hardwareBuffer!!, FrameInfo(...))
            if (status == IstmoSubmitStatus.CLOSED) stopCapture()
        }
    }, handler)
}
```

```swift
func start(frames: IstmoFrameSink<FrameInfo>) async throws {
    output.onSample = { sample in
        guard let pixels = CMSampleBufferGetImageBuffer(sample) else { return }
        if frames.submit(pixels, meta: FrameInfo(...)) == .closed { self.stop() }
    }
}
```

| Kotlin | Swift | What Rust gets |
|---|---|---|
| `submit(HardwareBuffer, meta)` (API 26+) | `submit(CVPixelBuffer, meta:)` / `submit(IOSurfaceRef, meta:)` | its own reference to the platform buffer; close yours whenever you like |
| `submit(ByteBuffer, length, meta)` (direct) | — | the buffer's memory, read in place: do not write it afterwards |
| `submitBytes(ByteArray, meta)` | `submitBytes(Data, meta:)` | one copy |
| `close()` | `close()` | end of stream |

Every submit returns `IstmoSubmitStatus`: `ACCEPTED`, `DISPLACED` (an
older frame was dropped), `FULL`, `CLOSED` (stop producing),
`UNKNOWN_SINK`, `BAD_META`, `UNSUPPORTED` (a `HardwareBuffer` below API
26), `NOT_STARTED`, `INVALID`. These go through four generic transport
entries (`nativeSubmitHardwareBuffer` & co. / `istmo_ios_submit_native_buffer`
& co.); no plugin adds JNI or C symbols of its own.

## Across processes

Sink ids only mean something in the runtime that opened them. A
producer in another process (a `:remote` service, or any other
runtime) fails to resolve the id — `IstmoError::UnknownFrameSink` in
Rust, `UNKNOWN_SINK` natively — and should fail the call with a domain
error. `runtime.shares_native_frames(plugin_id)` is `false` for plugins
declared remote, so a client can refuse before sending the id. Plugins
that want remote consumers offer an explicit copying path (a
`#[stream]` of encoded frames) next to the sink.

## Cost

Per item, same process (`crates/core/benches/stream_paths.rs`, release
build, Ryzen 9 9950X3D): a bus stream pays ~1.06 ms for an 8 MiB item
(two copies plus routing) against ~65–71 µs for the typed stream
shortcut or a sink, which is the producer's own allocation. For 16-byte
items every path is bound by thread hand-off (~330–490 ns); on one
thread the sink costs ~43 ns per frame, like an encode / decode round
trip, and a flume hand-off ~23 ns.

## Next

- Moving stream items without encoding: [Streams](/istmo/writing-plugins/streams/#in-process-shortcut).
- Objects the native side keeps: [Native handles](/istmo/writing-plugins/native-handles/).
