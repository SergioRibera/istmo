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
serialised: DMA-BUFs, `HardwareBuffer`s, `CVPixelBuffer`s / `IOSurface`s
/ `CMSampleBuffer`s, D3D11 textures, or plain CPU memory you already
have.

The consumer opens a sink and passes its id to the plugin as an
ordinary method argument. The producer — a Rust host in the same
process, or a Kotlin / Swift backend — pushes **frames** into it: plugin
metadata plus a `NativeBuffer`. Nothing is encoded but the metadata (and
not even that from Rust), and dropping a frame releases its buffer to
the producer.

## A whole round trip

The Rust snippets on this page are compiled as tests, so they stay
correct.

```rust
use std::sync::Arc;

use istmo::{FrameSinkId, HostCtx, NativeBuffer, Runtime, SinkPolicy};

#[istmo::message]
#[derive(Debug, Clone)]
pub struct FrameInfo {
    pub width: u32,
    pub height: u32,
    pub timestamp_ns: u64,
}

#[istmo::message]
#[derive(Debug, Clone)]
pub enum CaptureError {
    SinkUnreachable,
}

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the frame sink is not reachable from this host")
    }
}

impl std::error::Error for CaptureError {}

#[istmo::plugin(name = "acme.capture")]
pub trait Capture {
    /// Capture `count` frames into `frames`.
    async fn start(
        &self,
        ctx: HostCtx,
        count: u32,
        frames: FrameSinkId<FrameInfo>,
    ) -> Result<(), CaptureError>;
}

/// The desktop backend: a Rust host in the consumer's process.
pub struct DesktopCapture;

impl Capture for DesktopCapture {
    async fn start(
        &self,
        ctx: HostCtx,
        count: u32,
        frames: FrameSinkId<FrameInfo>,
    ) -> Result<(), CaptureError> {
        // `HostCtx` is filled in by the host; it never crosses the wire.
        let sender = ctx
            .frame_sender(frames)
            .map_err(|_| CaptureError::SinkUnreachable)?;
        std::thread::spawn(move || {
            for n in 0..count {
                let info = FrameInfo { width: 4, height: 4, timestamp_ns: u64::from(n) };
                let pixels = NativeBuffer::from_bytes(vec![0u8; 4 * 4 * 4]);
                if let Err(err) = sender.push(info, pixels) {
                    if err.is_closed() {
                        return; // the consumer left: stop capturing
                    }
                }
            }
            sender.close(); // the consumer sees the end, `EndReason::Completed`
        });
        Ok(())
    }
}

fn main() -> Result<(), istmo::IstmoError> {
    let runtime: Arc<Runtime> = Runtime::mock().runtime;
    runtime.register_host(CaptureHost::new(DesktopCapture));
    let capture = CaptureClient::from_runtime(&runtime)?;

    let sink = runtime.open_frame_sink::<FrameInfo>(SinkPolicy::queue(4))?;
    pollster::block_on(capture.start(3, sink.id()))?;

    let mut seen = 0;
    while let Some(frame) = sink.recv() {
        if frame.dropped_before() > 0 {
            eprintln!("fell behind: {} frames evicted", frame.dropped_before());
        }
        assert_eq!(frame.buffer.as_bytes().map(<[u8]>::len), Some(64));
        seen += 1;
        // `frame` drops here: the producer gets its buffer back.
    }
    assert!(seen > 0);
    assert!(sink.end_reason().is_some_and(|reason| reason.is_completed()));
    Ok(())
}
```

`FrameSinkId<M>` is a `u64` on the wire; `M` names the metadata each
frame carries, so producer and consumer agree on it at compile time. A
sink id is only legal as a method argument, or an `Option` of one
(`audio: Option<FrameSinkId<AudioInfo>>` for an optional second
stream). `#[istmo::plugin]` rejects one anywhere else — a return value,
a `Vec`, a message field — at the offending type, together with every
other argument shape Kotlin and Swift cannot map (tuples, `usize`,
`Duration`, references, maps).

## Consuming

- `recv()` blocks the thread, `recv_async()` waits on any executor,
  `try_recv()` never waits. `FrameSink` is also a
  `futures_core::Stream` of frames, so `while let Some(frame) =
  sink.next().await` works with `StreamExt`, and `select!` with it.
- Every receive returns `None` once the sink ended and is drained;
  `end_reason()` then says why: `Completed` (a producer called
  `close()`), `Failed(error)` (`close_with(error)`, or a native
  `closeWithError`), `ProducerGone` (every producer dropped without
  closing — a crashed or forgotten producer), `ConsumerClosed`, or
  `Shutdown`.
- Several tasks may wait on one sink: each frame and the end wake all of
  them.
- Dropping the `FrameSink` closes it: producers are told on their next
  push and queued frames are released.

### Backpressure

Producers never block. The policy decides what a full sink does:

| Policy | Behaviour | Use for |
|---|---|---|
| `SinkPolicy::latest()` | depth 1, a new frame replaces the queued one | previews, mirroring |
| `SinkPolicy::queue(n)` | depth `n`, evict the oldest | encoders tolerating a hiccup |
| `SinkPolicy::queue(n).with_overflow(Overflow::DropNewest)` | depth `n`, refuse the new frame | consumers that need a contiguous prefix |

`queue(0)` panics: a sink that holds nothing would drop everything.
`stats()` counts frames `accepted`, `delivered`, `evicted` (lost to
`DropOldest`; the next frame's `dropped_before()` says how many),
`refused` (handed back by `DropNewest`; a producer that retries counts
once per attempt) and `discarded` (released undelivered when the
consumer dropped the sink or the runtime shut down).

Hold frames only as long as you need them: pooled producers
(`ImageReader`, a compositor's buffer pool) reuse a buffer only after
its frame drops, so a consumer that keeps every frame stalls capture.

## Producing from Rust

`ctx.frame_sender(id)` (or `runtime.frame_sender(id)`) resolves the id
into a typed `FrameSender`: no bytes involved. Every sender — each
resolution and each clone — counts as one producer, and the sink ends
as `ProducerGone` when the last one drops, so checking an id again never
closes a live stream. End it on purpose with `close()` or
`close_with(error)`.

A resolution fails with `IstmoError::UnknownFrameSink` (closed, or
opened by another runtime) or `FrameSinkTypeMismatch`, which names both
metadata types.

## Buffers

`NativeBuffer` owns one backing value plus release hooks. The backing's
own `Drop` releases what it holds; hooks added with `with_release` run
after it. Neither may unwind into the producer or the consumer: a panic
there is logged and the other releases still run. Cloning shares the
buffer, so fanning a frame out copies nothing; release runs once, when
the last clone drops.

| `kind()` | Backing | Made by |
|---|---|---|
| `Cpu` | any `AsRef<[u8]>` (`Vec<u8>`, `Bytes`, a mapping, a pool slot) — `as_bytes()` | `NativeBuffer::from_bytes` |
| `DmaBuf` | `Arc<DmaBuf>` (planes: `OwnedFd`, offset, stride, object `size`) — `dmabuf()` | `NativeBuffer::from_dmabuf` |
| `HardwareBuffer` | `AHardwareBuffer*` — `platform_ptr()` | Kotlin `IstmoFrameSender.submit(Image / HardwareBuffer, …)` |
| `CvPixelBuffer` / `IoSurface` / `CmSampleBuffer` | the retained object — `platform_ptr()` | Swift `IstmoFrameSender.submit(…)` |
| `D3d11Texture` | a plugin's own type implementing `PlatformBuffer` | `NativeBuffer::from_platform` |
| `VideoFrame` | reserved: web producers copy (`VideoFrame.copyTo`) into `from_bytes` for now | — |

`platform_ptr()` gives the platform object (with `kind()` saying which)
whoever produced it. `downcast_ref::<T>()` reaches a backing of a known
type, and `into_backing::<T>()` takes it out with a `BufferLease` that
keeps the producer's slot reserved until it drops (`IntoBackingError`
tells a shared buffer from a wrong type). Leases are `Send + Sync`.

**Synchronisation** is part of the hand-over: a producer pushes a frame
only once its writes are complete (it waited on its acquire fence), and
a consumer's reads are complete once it drops the buffer or its lease
(a GPU consumer waits for its own work first). No fence travels with
the frame.

**Pools.** A pooled producer keeps one `Arc<DmaBuf>` per slot and
pushes clones: no `dup` per frame, the fd numbers stay the same, and
`with_identity(BufferIdentity::new(generation, slot))` lets consumers
cache what they import per slot (a new generation when the pool is
reallocated). Plane fds must be close-on-exec (`O_CLOEXEC`,
`DRM_CLOEXEC`).

```rust
#[cfg(unix)]
mod pool {
    use std::sync::Arc;

    use istmo::{BufferIdentity, DmaBuf, NativeBuffer};

    /// Hand slot `slot` of a pooled producer to a consumer; `give_back`
    /// runs once every clone of the frame is gone.
    pub fn frame(
        generation: u64,
        slot: u32,
        pooled: &Arc<DmaBuf>,
        give_back: impl FnOnce() + Send + 'static,
    ) -> NativeBuffer {
        NativeBuffer::from_dmabuf(Arc::clone(pooled))
            .with_identity(BufferIdentity::new(generation, slot))
            .with_release(give_back)
    }
}
```

## Producing from Kotlin

The generated backend receives an `IstmoFrameSender<FrameInfo>` instead
of an id: one producer session, opened when the call arrives and ended
by `close()` / `closeWithError(message)` (or, as a safety net, when it is
garbage collected). Close it when the session ends.

```kotlin
override suspend fun start(count: UInt, frames: IstmoFrameSender<FrameInfo>) {
    // maxImages: what the consumer may hold, plus one being written.
    val reader = ImageReader.newInstance(w, h, PixelFormat.RGBA_8888, 4,
        HardwareBuffer.USAGE_GPU_SAMPLED_IMAGE or HardwareBuffer.USAGE_CPU_READ_RARELY)
    reader.setOnImageAvailableListener({ r ->
        val image = r.acquireLatestImage() ?: return@setOnImageAvailableListener
        // The sender keeps `image` open until Rust drops the frame, then
        // closes it: the producer never renders into a frame being read.
        val status = frames.submit(image, FrameInfo(w.toUInt(), h.toUInt(), image.timestamp.toULong()))
        if (status.shouldStop) { reader.close(); frames.close() }
    }, handler)
}
```

| Call | What Rust gets |
|---|---|
| `submit(image, meta)` (API 28+) | its own `AHardwareBuffer` reference; the `Image` stays open until Rust releases it (after its acquire fence, API 33+) |
| `submit(hardwareBuffer, meta, owner)` (API 26+) | its own reference; `owner` (if any) is closed once Rust releases it |
| `openSlotPool(size, count)`, `acquireSlot()`, `submit(slot, length, meta)` | Rust-owned memory Kotlin filled: no copy. A slot's `buffer` is valid only until it is submitted or `release(slot)`d |
| `submit(byteBuffer, meta)`, `submitBytes(bytes, meta)` | one copy; your buffer is yours again when the call returns |

## Producing from Swift

```swift
func start(count: UInt32, frames: IstmoFrameSender<FrameInfo>) async throws {
    output.onSample = { [weak self] sample in
        // Rust retains the sample buffer until the frame drops.
        let status = frames.submit(sample, meta: FrameInfo(width: w, height: h, timestampNs: 0))
        if status.shouldStop { self?.stop() }
    }
}
```

`IstmoFrameSender` is a `Sendable` class (submits are serialised), so
capture handlers on any queue can hold it; the session ends with
`close()` / `close(error:)` or when the sender is deinitialised.
`submit(CVPixelBuffer | IOSurfaceRef | CMSampleBuffer, meta:)` hand Rust a
retained reference; `submitBytes(_:meta:)` copies.

### Statuses

Every submit returns an `IstmoSubmitStatus`: `ACCEPTED`, `DISPLACED`
(taken; an older frame was evicted), `FULL` (refused), `CLOSED`,
`UNKNOWN_SINK`, `TYPE_MISMATCH` (the sender was generated for another
metadata type), `BAD_META`, `UNSUPPORTED`, `NOT_STARTED`, `INVALID`,
`INTERNAL` (a Rust bug; Rust releases what it took). `isTaken` says
whether the sink kept the frame, `shouldStop` whether every later submit
fails the same way.

All of this goes through a fixed set of transport entries
(`IstmoFrames.native*` on Android, `istmo_ios_*_frame_producer` and
`istmo_ios_submit_*` on Apple); no plugin adds JNI or C symbols. The
Kotlin and Swift runtimes check the native library's entry version at
start and refuse a mismatched one with a message naming both versions.

## Who can feed a sink

Sink ids are random 64-bit values, meaningful only in the runtime that
opened them. A generated host refuses a call carrying one unless it
comes from Rust code in the same process (`DispatchError::OriginRefused`
for Kotlin, Swift or another process); a client refuses to send one to a
plugin declared remote (`IstmoError::FrameSinkNotShareable`); and
generated Kotlin / Swift clients have no methods taking sinks. A native
producer presents a fingerprint of its metadata type when it opens the
sink, so one generated for another type gets `TYPE_MISMATCH` instead of
feeding bytes that happen to decode.

Plugins that want consumers in another process offer an explicit copying
path (a `#[stream]` of encoded frames) next to the sink.

## Cost

Release build, Ryzen 9 9950X3D, same process
(`crates/core/benches/stream_paths.rs`):

| | 16 B item | 8 MiB item |
|---|---|---|
| bus stream (encode, route, decode) | ~390 ns | ~1.06 ms |
| typed stream shortcut / frame sink | ~350–425 ns | ~60–76 µs (the producer's own allocation) |
| frame sink, pooled buffers | | ~18 µs |

On one thread a push plus receive costs ~50–60 ns (two allocations:
the buffer and its `Vec`); a native producer's submit, metadata decoded,
~60 ns, and it scales with threads like a Rust producer (no global lock).

## Next

- Moving stream items without encoding: [Streams](/istmo/writing-plugins/streams/#in-process-shortcut).
- Objects the native side keeps: [Native handles](/istmo/writing-plugins/native-handles/).
