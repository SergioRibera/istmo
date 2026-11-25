//! The typed in-process shortcut for `#[stream]` methods: the client
//! gets the implementation's own receiver, nothing reaches the wire, and
//! completion, failure, cancellation and panics behave like the wire
//! path.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use istmo::{CancelToken, IstmoError, Runtime, StreamItem};

#[istmo::message]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub n: u32,
    pub payload: Vec<u8>,
}

#[istmo::message]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensorError(pub String);

impl core::fmt::Display for SensorError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SensorError {}

#[istmo::plugin(name = "test.typed.sensor")]
pub trait Sensor {
    /// `count` samples, then the end.
    #[istmo::stream]
    fn samples(&self, count: u32) -> Sample;

    /// `ok` samples, then a failure.
    #[istmo::stream]
    fn flaky(&self, ok: u32) -> Result<Sample, SensorError>;

    /// Samples until cancelled.
    #[istmo::stream]
    fn forever(&self, cancel: CancelToken) -> u32;

    async fn ping(&self) -> u32;
}

#[derive(Default)]
struct Backend {
    /// Set by `forever`'s producer once it saw its token trip.
    saw_cancel: Arc<AtomicBool>,
    panic_on_open: bool,
}

impl Sensor for Backend {
    fn samples(&self, count: u32) -> flume::Receiver<Sample> {
        assert!(!self.panic_on_open, "boom from typed stream host");
        let (tx, rx) = flume::unbounded();
        for n in 0..count {
            tx.send(Sample {
                n,
                payload: vec![u8::try_from(n % 256).unwrap_or(0); 4],
            })
            .unwrap();
        }
        rx
    }

    fn flaky(&self, ok: u32) -> flume::Receiver<Result<Sample, SensorError>> {
        let (tx, rx) = flume::unbounded();
        for n in 0..ok {
            tx.send(Ok(Sample { n, payload: vec![] })).unwrap();
        }
        tx.send(Err(SensorError("sensor unplugged".to_owned())))
            .unwrap();
        // Ignored: nothing after the first error is read.
        tx.send(Ok(Sample {
            n: 99,
            payload: vec![],
        }))
        .unwrap();
        rx
    }

    fn forever(&self, cancel: CancelToken) -> flume::Receiver<u32> {
        let (tx, rx) = flume::bounded(1);
        let saw_cancel = Arc::clone(&self.saw_cancel);
        std::thread::spawn(move || {
            let mut n = 0;
            while !cancel.is_cancelled() {
                if tx.send_timeout(n, Duration::from_millis(5)).is_ok() {
                    n += 1;
                }
            }
            saw_cancel.store(true, Ordering::SeqCst);
        });
        rx
    }

    async fn ping(&self) -> u32 {
        1
    }
}

fn runtime_with(backend: Backend) -> (Arc<Runtime>, flume::Receiver<istmo::Envelope>) {
    let init = Runtime::mock();
    SensorHost::new(backend).register(&init.runtime);
    (init.runtime, init.outbound)
}

#[test]
fn stream_items_are_moved_without_touching_the_wire() {
    let (rt, outbound) = runtime_with(Backend::default());
    let client = SensorClient::from_runtime(&rt).expect("client");
    let stream = client.samples(3).expect("open");
    assert!(stream.is_local());
    assert!(stream.into_wire().is_none());

    let stream = client.samples(3).expect("open");
    for n in 0..3 {
        match stream.recv().expect("item") {
            StreamItem::Event(sample) => assert_eq!(sample.n, n),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(stream.recv().expect("end"), StreamItem::Completed);
    assert!(matches!(stream.recv(), Err(IstmoError::ChannelClosed)));
    assert!(
        outbound.is_empty(),
        "nothing went through the outbound channel"
    );
}

#[test]
fn async_and_try_recv_see_the_same_items() {
    let (rt, _outbound) = runtime_with(Backend::default());
    let client = SensorClient::from_runtime(&rt).expect("client");
    let stream = client.samples(2).expect("open");
    assert!(matches!(
        stream.try_recv(),
        Some(Ok(StreamItem::Event(Sample { n: 0, .. })))
    ));
    let next = pollster::block_on(stream.recv_async()).expect("item");
    assert!(matches!(next, StreamItem::Event(Sample { n: 1, .. })));
    assert_eq!(
        pollster::block_on(stream.recv_async()).expect("end"),
        StreamItem::Completed
    );
}

#[test]
fn fallible_streams_end_at_the_first_error() {
    let (rt, _outbound) = runtime_with(Backend::default());
    let client = SensorClient::from_runtime(&rt).expect("client");
    let stream = client.flaky(1).expect("open");
    assert!(matches!(
        stream.recv().expect("item"),
        StreamItem::Event(Sample { n: 0, .. })
    ));
    assert_eq!(
        stream.recv().expect("failure"),
        StreamItem::Failed(SensorError("sensor unplugged".to_owned()))
    );
    assert!(matches!(stream.recv(), Err(IstmoError::ChannelClosed)));
}

#[test]
fn dropping_the_stream_trips_the_producers_token() {
    let backend = Backend::default();
    let saw_cancel = Arc::clone(&backend.saw_cancel);
    let (rt, _outbound) = runtime_with(backend);
    let client = SensorClient::from_runtime(&rt).expect("client");
    let stream = client.forever().expect("open");
    assert!(matches!(stream.recv().expect("item"), StreamItem::Event(0)));
    drop(stream);
    for _ in 0..200 {
        if saw_cancel.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the producer never saw its token trip");
}

#[test]
fn shutdown_cancels_a_waiting_consumer() {
    let (rt, _outbound) = runtime_with(Backend::default());
    let client = SensorClient::from_runtime(&rt).expect("client");
    let stream = client.forever().expect("open");
    let shutdown = {
        let rt = Arc::clone(&rt);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            rt.shutdown();
        })
    };
    let ended = loop {
        match stream.recv().expect("item") {
            StreamItem::Event(_) => {}
            other => break other,
        }
    };
    shutdown.join().unwrap();
    assert_eq!(ended, StreamItem::Cancelled);
    assert!(matches!(
        client.samples(1),
        Err(IstmoError::RuntimeShutDown)
    ));
}

#[test]
fn a_panicking_host_fails_the_open_like_the_unary_shortcut() {
    let (rt, _outbound) = runtime_with(Backend {
        panic_on_open: true,
        ..Backend::default()
    });
    let client = SensorClient::from_runtime(&rt).expect("client");
    match client.samples(1) {
        Err(IstmoError::PluginError { bytes }) => {
            let (msg, _) = istmo::codec::decode::<String>(&bytes).expect("panic marker");
            assert_eq!(msg, "host panicked");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn wire_path_still_serves_streams_without_the_shortcut() {
    let init = Runtime::mock();
    init.runtime
        .register_host(SensorHost::new(Backend::default()));
    let client = SensorClient::from_runtime(&init.runtime).expect("client");
    let stream = client.samples(2).expect("open");
    assert!(!stream.is_local());
    let mut seen = Vec::new();
    loop {
        match stream.recv().expect("item") {
            StreamItem::Event(sample) => seen.push(sample.n),
            StreamItem::Completed => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(seen, [0, 1]);
}
