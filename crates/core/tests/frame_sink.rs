//! Native frame sinks: policies, drop accounting, release, lifetime and
//! the type-erased native submit.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use istmo_core::native_frame::{
    MemoryKind, NativeBuffer, Overflow, PushError, Pushed, SinkPolicy, SinkStats, SubmitStatus,
};
use istmo_core::{FrameSinkId, IstmoError, Runtime, codec};

/// A buffer that counts its releases.
fn counted(released: &Arc<AtomicUsize>) -> NativeBuffer {
    let released = Arc::clone(released);
    NativeBuffer::from_cpu(vec![0u8; 8]).with_release(move || {
        released.fetch_add(1, Ordering::SeqCst);
    })
}

const fn depth(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).expect("non-zero")
}

#[test]
fn latest_keeps_the_newest_frame_and_counts_the_rest() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    let sender = rt.frame_sender(sink.id()).expect("sender");
    let released = Arc::new(AtomicUsize::new(0));

    assert_eq!(
        sender.push(1, counted(&released)).expect("push"),
        Pushed::Queued
    );
    assert_eq!(
        sender.push(2, counted(&released)).expect("push"),
        Pushed::Displaced
    );
    assert_eq!(
        sender.push(3, counted(&released)).expect("push"),
        Pushed::Displaced
    );
    assert_eq!(
        released.load(Ordering::SeqCst),
        2,
        "evicted frames are released at once"
    );

    let frame = sink.try_recv().expect("newest");
    assert_eq!((frame.meta, frame.seq(), frame.dropped_before()), (3, 2, 2));
    assert!(sink.try_recv().is_none());
    drop(frame);
    assert_eq!(released.load(Ordering::SeqCst), 3);
    assert_eq!(
        sink.stats(),
        SinkStats {
            accepted: 3,
            delivered: 1,
            dropped: 2
        }
    );
}

#[test]
fn drop_newest_refuses_and_hands_the_frame_back() {
    let rt = Runtime::mock().runtime;
    let policy = SinkPolicy::queue(depth(2)).with_overflow(Overflow::DropNewest);
    let sink = rt.open_frame_sink::<u32>(policy).expect("open");
    let sender = rt.frame_sender(sink.id()).expect("sender");
    sender.push(1, NativeBuffer::from_cpu([1u8])).expect("push");
    sender.push(2, NativeBuffer::from_cpu([2u8])).expect("push");
    match sender.push(3, NativeBuffer::from_cpu([3u8])) {
        Err(PushError::Full(meta, buffer)) => {
            assert_eq!(meta, 3);
            assert_eq!(buffer.cpu_bytes(), Some(&[3u8][..]));
        }
        other => panic!("unexpected {other:?}"),
    }
    let first = sink.recv().expect("first");
    assert_eq!((first.meta, first.dropped_before()), (1, 1));
    let second = sink.recv().expect("second");
    assert_eq!((second.meta, second.dropped_before()), (2, 0));
}

#[test]
fn queue_evicts_the_oldest_in_order() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::queue(depth(3)))
        .expect("open");
    let sender = rt.frame_sender(sink.id()).expect("sender");
    for n in 0..5 {
        sender
            .push(n, NativeBuffer::from_cpu(Vec::new()))
            .expect("push");
    }
    let got: Vec<u32> = std::iter::from_fn(|| sink.try_recv().map(|f| f.meta)).collect();
    assert_eq!(got, [2, 3, 4]);
}

#[test]
fn dropping_the_consumer_closes_producers_and_releases_queued_frames() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::queue(depth(4)))
        .expect("open");
    let id = sink.id();
    let sender = rt.frame_sender(id).expect("sender");
    let released = Arc::new(AtomicUsize::new(0));
    sender.push(1, counted(&released)).expect("push");
    sender.push(2, counted(&released)).expect("push");
    drop(sink);
    assert_eq!(released.load(Ordering::SeqCst), 2);
    assert!(sender.is_closed());
    assert_eq!(
        rt.submit_native_frame(
            id.raw(),
            &codec::encode(&3u32).expect("meta"),
            counted(&released)
        ),
        SubmitStatus::Closed,
        "a native producer hears the consumer left"
    );
    let refused = sender.push(3, counted(&released)).expect_err("closed");
    assert!(refused.is_closed());
    drop(refused);
    assert_eq!(released.load(Ordering::SeqCst), 4);
    assert!(matches!(
        rt.frame_sender(id),
        Err(IstmoError::UnknownFrameSink(_))
    ));
}

#[test]
fn producer_close_delivers_what_is_queued_then_ends() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::queue(depth(4)))
        .expect("open");
    let sender = rt.frame_sender(sink.id()).expect("sender");
    let clone = sender.clone();
    sender
        .push(1, NativeBuffer::from_cpu(Vec::new()))
        .expect("push");
    drop(sender);
    assert!(!sink.is_closed(), "a clone of the producer still lives");
    drop(clone);
    assert!(sink.is_closed());
    assert_eq!(sink.recv().map(|f| f.meta), Some(1));
    assert!(sink.recv().is_none());
}

#[test]
fn async_consumer_wakes_on_frames_from_another_thread() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u64>(SinkPolicy::queue(depth(8)))
        .expect("open");
    let sender = rt.frame_sender(sink.id()).expect("sender");
    let producer = std::thread::spawn(move || {
        for n in 0..100u64 {
            if n % 10 == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
            while let Err(PushError::Full(..)) = sender.push(n, NativeBuffer::from_cpu(Vec::new()))
            {
                std::thread::yield_now();
            }
        }
    });
    let mut last = None;
    let mut dropped_total = 0;
    pollster::block_on(async {
        while let Some(frame) = sink.recv_async().await {
            dropped_total += frame.dropped_before();
            last = Some(frame.meta);
        }
    });
    producer.join().unwrap();
    assert_eq!(last, Some(99));
    assert_eq!(sink.stats().delivered + dropped_total, 100);
}

#[test]
fn shutdown_closes_every_sink() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    let sender = rt.frame_sender(sink.id()).expect("sender");
    let released = Arc::new(AtomicUsize::new(0));
    sender.push(1, counted(&released)).expect("push");
    let waiter = std::thread::spawn(move || sink.recv().map(|f| f.meta));
    std::thread::sleep(Duration::from_millis(10));
    rt.shutdown();
    // The queued frame is released at shutdown, or was received first.
    let got = waiter.join().unwrap();
    assert!(got.is_none() || got == Some(1));
    assert_eq!(released.load(Ordering::SeqCst), 1);
    assert!(sender.is_closed());
    assert!(matches!(
        rt.open_frame_sink::<u32>(SinkPolicy::latest()),
        Err(IstmoError::RuntimeShutDown)
    ));
}

#[test]
fn sender_lookup_checks_the_metadata_type() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    let wrong = FrameSinkId::<String>::from_raw(sink.id().raw());
    assert!(matches!(
        rt.frame_sender(wrong),
        Err(IstmoError::FrameSinkTypeMismatch(_))
    ));
    assert!(matches!(
        rt.frame_sender(FrameSinkId::<u32>::from_raw(u64::MAX)),
        Err(IstmoError::UnknownFrameSink(u64::MAX))
    ));
}

#[test]
fn native_submits_decode_the_metadata_and_report_a_status() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<(u32, String)>(SinkPolicy::latest())
        .expect("open");
    let raw = sink.id().raw();
    let meta = codec::encode(&(7u32, "nv12".to_owned())).expect("encode");
    let released = Arc::new(AtomicUsize::new(0));

    assert_eq!(
        rt.submit_native_frame(raw, &meta, counted(&released)),
        SubmitStatus::Accepted
    );
    assert_eq!(
        rt.submit_native_frame(raw, &meta, counted(&released)),
        SubmitStatus::Displaced
    );
    assert_eq!(
        rt.submit_native_frame(raw, &[0xff], counted(&released)),
        SubmitStatus::BadMeta
    );
    assert_eq!(
        rt.submit_native_frame(raw + 1000, &meta, counted(&released)),
        SubmitStatus::UnknownSink
    );
    assert_eq!(
        released.load(Ordering::SeqCst),
        3,
        "refused buffers are released"
    );

    let frame = sink.try_recv().expect("frame");
    assert_eq!(frame.meta, (7, "nv12".to_owned()));
    assert_eq!(frame.buffer.kind(), MemoryKind::Cpu);

    assert!(rt.close_frame_sink(raw));
    assert_eq!(
        rt.submit_native_frame(raw, &meta, counted(&released)),
        SubmitStatus::Closed
    );
    assert!(!rt.close_frame_sink(raw + 1000));
    assert_eq!(SubmitStatus::Closed.code(), 2);
    assert!(SubmitStatus::Displaced.is_taken());
}

#[test]
fn remote_plugins_do_not_share_native_frames() {
    let rt = Runtime::mock().runtime;
    assert!(rt.shares_native_frames("local.p"));
    rt.declare_remote_plugin("remote.p");
    assert!(!rt.shares_native_frames("remote.p"));
}
