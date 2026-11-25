//! Per-item cost of the three ways a Rust producer can reach a Rust
//! consumer in the same process:
//!
//! 1. `bus` — a `#[stream]` method served through the wire path: the
//!    host's encoder thread encodes each item, the runtime pumps it into
//!    the stream's routing slot, the consumer decodes it.
//! 2. `typed` — the same method through the typed in-process shortcut:
//!    the consumer receives the producer's own channel, items are moved.
//! 3. `sink` — a frame sink: the producer pushes metadata plus a
//!    [`NativeBuffer`] through a `FrameSender`, the consumer receives them
//!    moved.
//! 4. `sink_native` — the frame sink fed the way the Android / iOS entry
//!    points feed it: metadata wire-encoded, decoded once by the sink
//!    (a `NativeProducer` resolved once per session), buffer moved.
//!
//! Two item sizes: 16 bytes (control / sample traffic) and 8 MiB (a
//! 1080p-to-4K raw frame). The producer allocates each payload with
//! `vec![0; n]` in every variant, so the difference between rows is
//! the transport alone. One iteration streams a whole batch on a
//! producer thread; criterion's per-element throughput is the per-item
//! cost.

use std::hint::black_box;
use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use istmo_core::native_frame::{NativeBuffer, Overflow, PushError, SinkPolicy, meta_fingerprint};
use istmo_core::{Runtime, StreamItem, codec};
use istmo_macros::{message, plugin};

/// Producer-side channel bound, the same in every variant.
const DEPTH: usize = 64;

/// One stream item.
#[message(crate = "::istmo_core")]
#[derive(Debug)]
pub struct Blob {
    pub seq: u32,
    pub bytes: Vec<u8>,
}

/// Frame metadata for the sink variants.
#[message(crate = "::istmo_core")]
#[derive(Debug)]
pub struct Meta {
    pub seq: u32,
}

#[plugin(name = "bench.source", crate = "::istmo_core")]
pub trait Source {
    #[istmo_macros::stream]
    fn items(&self, count: u32, size: u32) -> Blob;
}

struct Producer;

impl Source for Producer {
    fn items(&self, count: u32, size: u32) -> flume::Receiver<Blob> {
        let (tx, rx) = flume::bounded(DEPTH);
        std::thread::spawn(move || {
            for seq in 0..count {
                let bytes = vec![0u8; size as usize];
                if tx.send(Blob { seq, bytes }).is_err() {
                    break;
                }
            }
        });
        rx
    }
}

/// `(label, item size, items per iteration)`.
const CASES: [(&str, u32, u32); 2] = [("16B", 16, 2_000), ("8MiB", 8 << 20, 16)];

fn drain(client: &SourceClient, count: u32, size: u32) {
    let stream = client.items(count, size).expect("open stream");
    let mut seen = 0;
    loop {
        match stream.recv().expect("item") {
            StreamItem::Event(blob) => {
                black_box(blob.bytes.len());
                seen += 1;
            }
            StreamItem::Completed => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(seen, count);
}

fn bus(c: &mut Criterion) {
    let rt = Runtime::mock().runtime;
    rt.register_host_wire_only(SourceHost::new(Producer));
    let client = SourceClient::from_runtime(&rt).expect("client");
    run_stream_group(c, "stream_paths/bus", &client, false);
}

fn typed(c: &mut Criterion) {
    let rt = Runtime::mock().runtime;
    rt.register_host(SourceHost::new(Producer));
    let client = SourceClient::from_runtime(&rt).expect("client");
    run_stream_group(c, "stream_paths/typed", &client, true);
}

fn run_stream_group(c: &mut Criterion, name: &str, client: &SourceClient, local: bool) {
    assert_eq!(client.items(0, 0).expect("probe").is_local(), local);
    let mut group = c.benchmark_group(name);
    for (label, size, count) in CASES {
        group.throughput(Throughput::Elements(u64::from(count)));
        group.bench_function(BenchmarkId::from_parameter(label), |b| {
            b.iter(|| drain(client, count, size));
        });
    }
    group.finish();
}

/// Push `count` frames from a producer thread into a sink deep enough to
/// take the whole batch, so no frame is lost (as the stream variants lose
/// none) and the producer never waits: the row measures the hand-off,
/// not a retry loop. The retry is there for correctness only.
fn sink_batch(rt: &Arc<Runtime>, count: u32, size: u32, native: bool) {
    let policy = SinkPolicy::queue(count as usize).with_overflow(Overflow::DropNewest);
    let sink = rt.open_frame_sink::<Meta>(policy).expect("open sink");
    let producer = {
        let rt = Arc::clone(rt);
        let id = sink.id();
        std::thread::spawn(move || {
            let sender = rt.frame_sender(id).expect("sender");
            let native_producer = rt
                .open_native_producer(id.raw(), meta_fingerprint::<Meta>())
                .expect("native producer");
            drop(rt);
            for seq in 0..count {
                let buffer = NativeBuffer::from_bytes(vec![0u8; size as usize]);
                if native {
                    let meta = codec::encode(&Meta { seq }).expect("encode meta");
                    loop {
                        let status = native_producer.submit(&meta, buffer.clone());
                        if status.is_taken() {
                            break;
                        }
                        std::thread::yield_now();
                    }
                    drop(buffer);
                    continue;
                }
                let (mut meta, mut buffer) = (Meta { seq }, buffer);
                loop {
                    match sender.push(meta, buffer) {
                        Ok(_) => break,
                        Err(PushError::Full(m, b)) => {
                            (meta, buffer) = (m, b);
                            std::thread::yield_now();
                        }
                        Err(_) => return,
                    }
                }
            }
        })
    };
    let mut seen = 0;
    while let Some(frame) = sink.recv() {
        black_box(frame.buffer.as_bytes().map(<[u8]>::len));
        seen += 1;
    }
    producer.join().expect("producer");
    assert_eq!(seen, count);
}

fn sink(c: &mut Criterion) {
    let rt = Runtime::mock().runtime;
    for (name, native) in [
        ("stream_paths/sink", false),
        ("stream_paths/sink_native", true),
    ] {
        let mut group = c.benchmark_group(name);
        for (label, size, count) in CASES {
            group.throughput(Throughput::Elements(u64::from(count)));
            group.bench_function(BenchmarkId::from_parameter(label), |b| {
                b.iter(|| sink_batch(&rt, count, size, native));
            });
        }
        group.finish();
    }
}

/// One item handed over and taken back on a single thread: the CPU cost
/// of each mechanism without scheduler wake-ups.
fn handoff(c: &mut Criterion) {
    let mut group = c.benchmark_group("stream_paths/handoff_16B");
    group.throughput(Throughput::Elements(1));
    let (tx, rx) = flume::bounded::<Blob>(DEPTH);
    group.bench_function("typed_channel", |b| {
        b.iter(|| {
            tx.send(Blob {
                seq: 1,
                bytes: vec![0u8; 16],
            })
            .expect("send");
            black_box(rx.recv().expect("recv").bytes.len());
        });
    });
    let (wire_tx, wire_rx) = flume::bounded::<Vec<u8>>(DEPTH);
    group.bench_function("bus_encode_channel_decode", |b| {
        b.iter(|| {
            let bytes = codec::encode(&Blob {
                seq: 1,
                bytes: vec![0u8; 16],
            })
            .expect("encode");
            wire_tx.send(bytes).expect("send");
            let bytes = wire_rx.recv().expect("recv");
            let (blob, _) = codec::decode::<Blob>(&bytes).expect("decode");
            black_box(blob.bytes.len());
        });
    });
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<Meta>(SinkPolicy::queue(DEPTH))
        .expect("open sink");
    let sender = rt.frame_sender(sink.id()).expect("sender");
    group.bench_function("sink", |b| {
        b.iter(|| {
            sender
                .push(Meta { seq: 1 }, NativeBuffer::from_bytes(vec![0u8; 16]))
                .expect("push");
            black_box(
                sink.try_recv()
                    .expect("frame")
                    .buffer
                    .as_bytes()
                    .map(<[u8]>::len),
            );
        });
    });
    let meta = codec::encode(&Meta { seq: 1 }).expect("encode meta");
    let native_producer = rt
        .open_native_producer(sink.id().raw(), meta_fingerprint::<Meta>())
        .expect("native producer");
    group.bench_function("sink_native", |b| {
        b.iter(|| {
            let status = native_producer.submit(&meta, NativeBuffer::from_bytes(vec![0u8; 16]));
            assert!(status.is_taken());
            black_box(
                sink.try_recv()
                    .expect("frame")
                    .buffer
                    .as_bytes()
                    .map(<[u8]>::len),
            );
        });
    });
    group.finish();
}

criterion_group!(benches, handoff, bus, typed, sink);
criterion_main!(benches);
