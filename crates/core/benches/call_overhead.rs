//! Per-call overhead benchmark.
//!
//! Separates the three cost centers a user-visible istmo call pays on a
//! native target:
//!
//! 1. Codec roundtrip (`codec::encode` + `codec::decode`).
//! 2. One bare `std::thread` spawn + join — the executor cost
//!    [`runtime::exec::spawn`] pays per hosted call on native.
//! 3. End-to-end in-process `Runtime::call` → `recv_blocking` roundtrip.
//! 4. `dispatch_inbound` + outbound `Envelope::to_wire_bytes` — the path
//!    an inbound call from Kotlin/Swift travels, bytes and all.
//!
//! Payload sizes: 0, 64, 1 KiB, 64 KiB.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use istmo_core::{
    CallId, CancelToken, Dispatch, DispatchFuture, Envelope, Frame, InstanceId, Outcome, Runtime,
    codec,
};

const SIZES: [usize; 4] = [0, 64, 1 << 10, 1 << 16];

struct NoopDispatch {
    plugin_id: &'static str,
    count: AtomicBool,
}

impl NoopDispatch {
    const fn new(plugin_id: &'static str) -> Self {
        Self {
            plugin_id,
            count: AtomicBool::new(false),
        }
    }
}

impl Dispatch for NoopDispatch {
    fn plugin_id(&self) -> &'static str {
        self.plugin_id
    }

    fn dispatch<'a>(
        &'a self,
        _instance: Option<InstanceId>,
        _method: &'a str,
        payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        let out = payload.to_vec();
        self.count.store(true, Ordering::Relaxed);
        Box::pin(async move { Ok(Outcome::Ok(out)) })
    }
}

fn mk_payload(size: usize) -> Vec<u8> {
    (0..size).map(|i| i as u8).collect()
}

fn codec_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("codec_roundtrip_vec_u8");
    for &size in &SIZES {
        let payload = mk_payload(size);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(format!("{size}B"), |b| {
            b.iter(|| {
                let bytes = codec::encode(&payload).unwrap();
                let (out, _): (Vec<u8>, _) = codec::decode(&bytes).unwrap();
                std::hint::black_box(out);
            });
        });
    }
    group.finish();
}

fn envelope_wire_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("envelope_wire_roundtrip");
    for &size in &SIZES {
        let env = Envelope::new(Frame::Call {
            call_id: CallId(1),
            plugin_id: "bench.noop".to_owned(),
            instance_id: None,
            method: "ping".to_owned(),
            payload: mk_payload(size),
        });
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(format!("{size}B"), |b| {
            b.iter(|| {
                let bytes = env.to_wire_bytes().unwrap();
                let decoded = Envelope::from_wire_bytes(&bytes).unwrap();
                std::hint::black_box(decoded);
            });
        });
    }
    group.finish();
}

fn thread_spawn_join_baseline(c: &mut Criterion) {
    c.bench_function("thread_spawn_join_baseline", |b| {
        b.iter(|| {
            let h = std::thread::spawn(|| std::hint::black_box(0u32));
            h.join().unwrap();
        });
    });
}

fn in_process_hosted_call(c: &mut Criterion) {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    // Drain outbound: hosted calls short-circuit, but the mock init
    // allocates a bounded channel we want to keep empty for other paths.
    let _outbound = init.outbound;
    rt.register_host(NoopDispatch::new("bench.noop"));

    let mut group = c.benchmark_group("hosted_call_roundtrip");
    for &size in &SIZES {
        let payload = mk_payload(size);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(format!("{size}B"), |b| {
            b.iter(|| {
                let handle = rt
                    .call("bench.noop", None, "ping", payload.clone())
                    .expect("call");
                let result = handle.recv_blocking().expect("recv");
                let _ = std::hint::black_box(result);
            });
        });
    }
    group.finish();
}

fn dispatch_inbound_wire(c: &mut Criterion) {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    let outbound = init.outbound;
    rt.register_host(NoopDispatch::new("bench.noop"));

    let mut group = c.benchmark_group("dispatch_inbound_wire");
    let mut next_id: u64 = 1;
    for &size in &SIZES {
        let payload = mk_payload(size);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(format!("{size}B"), |b| {
            b.iter(|| {
                let call_id = CallId(next_id);
                next_id += 1;
                let env = Envelope::new(Frame::Call {
                    call_id,
                    plugin_id: "bench.noop".to_owned(),
                    instance_id: None,
                    method: "ping".to_owned(),
                    payload: payload.clone(),
                });
                // Simulates the JNI boundary: encode → inject bytes →
                // host runs → outbound envelope → encode back.
                let bytes = env.to_wire_bytes().unwrap();
                let decoded = Envelope::from_wire_bytes(&bytes).unwrap();
                rt.dispatch_inbound(decoded).expect("inbound");
                let reply = outbound
                    .recv_timeout(Duration::from_secs(2))
                    .expect("outbound");
                let reply_bytes = reply.to_wire_bytes().unwrap();
                std::hint::black_box(reply_bytes);
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    codec_roundtrip,
    envelope_wire_roundtrip,
    thread_spawn_join_baseline,
    in_process_hosted_call,
    dispatch_inbound_wire,
);
criterion_main!(benches);
