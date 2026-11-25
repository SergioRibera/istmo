//! Frame sinks end to end: policies, producers, ends, native producers,
//! shutdown and the races between them.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use istmo_core::native_frame::{NATIVE_ABI_VERSION, fingerprint_of, meta_fingerprint};
use istmo_core::{
    EndReason, FrameSinkId, IstmoError, MemoryKind, NativeBuffer, Overflow, PushError, Pushed,
    Runtime, SinkPolicy, SinkStats, SubmitStatus, codec,
};

fn counted(released: &Arc<AtomicUsize>) -> NativeBuffer {
    let released = Arc::clone(released);
    NativeBuffer::from_bytes(vec![0u8; 8]).with_release(move || {
        released.fetch_add(1, Ordering::SeqCst);
    })
}

fn empty() -> NativeBuffer {
    NativeBuffer::from_bytes(Vec::new())
}

#[test]
fn latest_keeps_only_the_newest_frame() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    let sender = rt.frame_sender(sink.id()).expect("sender");
    let released = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        sender.push(1, counted(&released)).ok(),
        Some(Pushed::Accepted)
    );
    assert_eq!(
        sender.push(2, counted(&released)).ok(),
        Some(Pushed::Displaced)
    );
    assert_eq!(
        released.load(Ordering::SeqCst),
        1,
        "the evicted frame is released"
    );
    let frame = sink.try_recv().expect("frame");
    assert_eq!((frame.meta, frame.seq(), frame.dropped_before()), (2, 1, 1));
    let stats = sink.stats();
    assert_eq!(
        (
            stats.accepted,
            stats.delivered,
            stats.evicted,
            stats.refused
        ),
        (2, 1, 1, 0)
    );
    assert_eq!(SinkStats::default().accepted, 0);
}

#[test]
fn drop_newest_refuses_and_counts_refusals_apart_from_losses() {
    let rt = Runtime::mock().runtime;
    let policy = SinkPolicy::queue(2).with_overflow(Overflow::DropNewest);
    let sink = rt.open_frame_sink::<u32>(policy).expect("open");
    let sender = rt.frame_sender(sink.id()).expect("sender");
    sender
        .push(1, NativeBuffer::from_bytes([1u8]))
        .expect("push");
    sender
        .push(2, NativeBuffer::from_bytes([2u8]))
        .expect("push");
    let mut retry = sender.push(3, NativeBuffer::from_bytes([3u8]));
    for _ in 0..5 {
        match retry {
            Err(PushError::Full(meta, buffer)) => {
                assert_eq!(buffer.as_bytes(), Some(&[3u8][..]));
                retry = sender.push(meta, buffer);
            }
            other => panic!("expected Full, got {other:?}"),
        }
    }
    drop(retry);
    let stats = sink.stats();
    assert_eq!((stats.accepted, stats.evicted, stats.refused), (2, 0, 6));
    let got: Vec<u32> = std::iter::from_fn(|| sink.try_recv().map(|f| f.meta)).collect();
    assert_eq!(got, [1, 2], "nothing the sink accepted was lost");
}

#[test]
fn queue_evicts_the_oldest() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::queue(3))
        .expect("open");
    let sender = rt.frame_sender(sink.id()).expect("sender");
    for n in 0..5 {
        sender.push(n, empty()).expect("push");
    }
    let got: Vec<u32> = std::iter::from_fn(|| sink.try_recv().map(|f| f.meta)).collect();
    assert_eq!(got, [2, 3, 4]);
}

#[test]
#[should_panic(expected = "depth of at least 1")]
fn a_zero_depth_queue_is_a_bug() {
    let _ = SinkPolicy::queue(0);
}

#[test]
fn dropping_the_consumer_closes_producers_and_releases_queued_frames() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::queue(4))
        .expect("open");
    let id = sink.id();
    let sender = rt.frame_sender(id).expect("sender");
    let native = rt
        .open_native_producer(id.raw(), meta_fingerprint::<u32>())
        .expect("producer");
    let released = Arc::new(AtomicUsize::new(0));
    sender.push(1, counted(&released)).expect("push");
    sender.push(2, counted(&released)).expect("push");
    drop(sink);
    assert_eq!(released.load(Ordering::SeqCst), 2);
    assert!(sender.is_closed());
    let meta = codec::encode(&3u32).expect("meta");
    assert_eq!(
        native.submit(&meta, counted(&released)),
        SubmitStatus::Closed
    );
    let refused = sender.push(3, counted(&released)).expect_err("closed");
    assert!(refused.is_closed());
    drop(refused);
    assert_eq!(released.load(Ordering::SeqCst), 4);
    assert!(matches!(
        rt.frame_sender(id),
        Err(IstmoError::UnknownFrameSink(_))
    ));
    assert_eq!(
        rt.open_native_producer(id.raw(), 0).err(),
        Some(SubmitStatus::Closed),
        "a late producer of a recently closed sink hears Closed"
    );
}

#[test]
fn every_producer_counts_and_the_last_one_ends_the_sink() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::queue(4))
        .expect("open");
    let producer = rt.frame_sender(sink.id()).expect("sender");
    // Resolving again (a probe, a second producer) and dropping it must
    // not end the live stream.
    drop(rt.frame_sender(sink.id()).expect("probe"));
    let native = rt.open_native_producer(sink.id().raw(), 0).expect("native");
    drop(native);
    producer.push(1, empty()).expect("still open");
    let clone = producer.clone();
    drop(producer);
    assert!(!sink.is_closed(), "a clone still produces");
    drop(clone);
    assert!(sink.is_closed());
    assert!(matches!(sink.end_reason(), Some(EndReason::ProducerGone)));
    assert_eq!(sink.recv().map(|f| f.meta), Some(1));
    assert!(sink.recv().is_none());
}

#[test]
fn end_reasons_tell_why_a_sink_ended() {
    #[derive(Debug)]
    struct DeviceLost;
    impl std::fmt::Display for DeviceLost {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("device lost")
        }
    }
    impl std::error::Error for DeviceLost {}

    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::queue(2))
        .expect("open");
    let sender = rt.frame_sender(sink.id()).expect("sender");
    assert!(sink.end_reason().is_none());
    sender.push(1, empty()).expect("push");
    sender.close_with(DeviceLost);
    sender.close();
    assert_eq!(sink.recv().map(|f| f.meta), Some(1));
    assert!(sink.recv().is_none());
    let reason = sink.end_reason().expect("ended");
    assert_eq!(
        reason.error().map(ToString::to_string).as_deref(),
        Some("device lost")
    );

    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    let sender = rt.frame_sender(sink.id()).expect("sender");
    sender.close();
    assert!(sink.end_reason().is_some_and(|r| r.is_completed()));

    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    sink.close();
    assert!(matches!(sink.end_reason(), Some(EndReason::ConsumerClosed)));

    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    let native = rt.open_native_producer(sink.id().raw(), 0).expect("native");
    native.close_with_message("camera unplugged");
    assert_eq!(
        sink.end_reason()
            .and_then(|r| r.error().map(ToString::to_string))
            .as_deref(),
        Some("camera unplugged")
    );
}

#[test]
fn async_consumer_wakes_on_frames_from_another_thread() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u64>(SinkPolicy::queue(8))
        .expect("open");
    let sender = rt.frame_sender(sink.id()).expect("sender");
    let producer = std::thread::spawn(move || {
        for n in 0..100u64 {
            if n % 10 == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
            while let Err(PushError::Full(..)) = sender.push(n, empty()) {
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
fn the_sink_is_a_stream() {
    use std::future::poll_fn;
    use std::pin::Pin;

    use futures_core::Stream;

    let rt = Runtime::mock().runtime;
    let mut sink = rt
        .open_frame_sink::<u32>(SinkPolicy::queue(8))
        .expect("open");
    let sender = rt.frame_sender(sink.id()).expect("sender");
    let producer = std::thread::spawn(move || {
        for n in 0..5 {
            std::thread::sleep(Duration::from_millis(1));
            sender.push(n, empty()).expect("push");
        }
    });
    let got = pollster::block_on(async {
        let mut got = Vec::new();
        while let Some(frame) = poll_fn(|cx| Pin::new(&mut sink).poll_next(cx)).await {
            got.push(frame.meta);
        }
        got
    });
    producer.join().unwrap();
    assert_eq!(got, [0, 1, 2, 3, 4]);
}

#[test]
fn every_waiting_receiver_wakes_at_the_end() {
    let rt = Runtime::mock().runtime;
    let sink = Arc::new(
        rt.open_frame_sink::<u32>(SinkPolicy::latest())
            .expect("open"),
    );
    let sender = rt.frame_sender(sink.id()).expect("sender");
    let waiters: Vec<_> = (0..3)
        .map(|n| {
            let sink = Arc::clone(&sink);
            std::thread::spawn(move || {
                if n == 0 {
                    pollster::block_on(sink.recv_async()).map(|f| f.meta)
                } else {
                    sink.recv().map(|f| f.meta)
                }
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(50));
    sender.close();
    for waiter in waiters {
        assert_eq!(waiter.join().unwrap(), None, "no receiver hangs");
    }
}

#[test]
fn a_second_waiter_gets_a_frame_queued_while_the_first_is_busy() {
    let rt = Runtime::mock().runtime;
    let sink = Arc::new(
        rt.open_frame_sink::<u32>(SinkPolicy::queue(4))
            .expect("open"),
    );
    let sender = rt.frame_sender(sink.id()).expect("sender");
    let (first, second) = (Arc::clone(&sink), Arc::clone(&sink));
    // Both wait; each takes one frame and stops receiving.
    let a = std::thread::spawn(move || first.recv().map(|f| f.meta));
    let b = std::thread::spawn(move || second.recv().map(|f| f.meta));
    std::thread::sleep(Duration::from_millis(50));
    sender.push(1, empty()).expect("push");
    std::thread::sleep(Duration::from_millis(20));
    sender.push(2, empty()).expect("push");
    let mut got = [a.join().unwrap(), b.join().unwrap()];
    got.sort_unstable();
    assert_eq!(got, [Some(1), Some(2)]);
}

#[test]
fn shutdown_closes_every_sink_and_late_producers_hear_closed() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    let id = sink.id();
    let sender = rt.frame_sender(id).expect("sender");
    let released = Arc::new(AtomicUsize::new(0));
    sender.push(1, counted(&released)).expect("push");
    let sink = Arc::new(sink);
    let waiter = {
        let sink = Arc::clone(&sink);
        std::thread::spawn(move || sink.recv().map(|f| f.meta))
    };
    std::thread::sleep(Duration::from_millis(10));
    rt.shutdown();
    let got = waiter.join().unwrap();
    assert!(got.is_none() || got == Some(1));
    assert_eq!(released.load(Ordering::SeqCst), 1);
    assert!(sender.is_closed());
    assert!(matches!(sink.end_reason(), Some(EndReason::Shutdown)));
    assert_eq!(
        rt.open_native_producer(id.raw(), 0).err(),
        Some(SubmitStatus::Closed)
    );
    assert!(matches!(
        rt.open_frame_sink::<u32>(SinkPolicy::latest()),
        Err(IstmoError::RuntimeShutDown)
    ));
}

#[test]
fn opening_sinks_while_shutting_down_never_leaves_one_open() {
    for _ in 0..50 {
        let rt = Runtime::mock().runtime;
        let opener = {
            let rt = Arc::clone(&rt);
            std::thread::spawn(move || {
                (0..20)
                    .filter_map(|_| rt.open_frame_sink::<u32>(SinkPolicy::latest()).ok())
                    .collect::<Vec<_>>()
            })
        };
        rt.shutdown();
        for sink in opener.join().unwrap() {
            assert!(
                sink.is_closed(),
                "every sink returned before shutdown was closed by it"
            );
        }
    }
}

#[test]
fn sender_lookup_names_both_metadata_types() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    let wrong = FrameSinkId::<String>::from_raw(sink.id().raw());
    let err = rt.frame_sender(wrong).expect_err("mismatch");
    let message = err.to_string();
    assert!(
        message.contains("u32") && message.contains("String"),
        "{message}"
    );
    assert!(matches!(
        rt.frame_sender(FrameSinkId::<u32>::from_raw(u64::MAX)),
        Err(IstmoError::UnknownFrameSink(u64::MAX))
    ));
}

#[test]
fn sink_ids_are_random_and_unique_across_runtimes() {
    let a = Runtime::mock().runtime;
    let b = Runtime::mock().runtime;
    let sinks: Vec<_> = (0..64)
        .map(|_| {
            a.open_frame_sink::<u32>(SinkPolicy::latest())
                .expect("open")
        })
        .collect();
    let other = b
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    let ids: Vec<u64> = sinks.iter().map(|s| s.id().raw()).collect();
    let sequential = ids
        .windows(2)
        .filter(|w| w[1] == w[0].wrapping_add(1))
        .count();
    assert_eq!(sequential, 0, "ids are not a counter: {ids:x?}");
    assert!(!ids.contains(&other.id().raw()));
    // An id from one runtime resolves to nothing in another.
    assert!(matches!(
        b.frame_sender(sinks[0].id()),
        Err(IstmoError::UnknownFrameSink(_))
    ));
}

#[test]
fn native_producers_check_the_type_then_decode_each_frame() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<(u32, String)>(SinkPolicy::latest())
        .expect("open");
    let raw = sink.id().raw();
    assert_eq!(
        rt.open_native_producer(raw, meta_fingerprint::<(u64, u8)>())
            .err(),
        Some(SubmitStatus::TypeMismatch),
        "a producer generated for another metadata type is refused up front"
    );
    assert_eq!(
        rt.open_native_producer(raw ^ 1, 0).err(),
        Some(SubmitStatus::UnknownSink)
    );
    let producer = rt
        .open_native_producer(raw, meta_fingerprint::<(u32, String)>())
        .expect("producer");
    assert_eq!(producer.sink_id(), raw);
    let meta = codec::encode(&(7u32, "nv12".to_owned())).expect("encode");
    let released = Arc::new(AtomicUsize::new(0));

    assert_eq!(
        producer.submit(&meta, counted(&released)),
        SubmitStatus::Accepted
    );
    assert_eq!(
        producer.submit(&meta, counted(&released)),
        SubmitStatus::Displaced
    );
    assert_eq!(
        producer.submit(&[0xff], counted(&released)),
        SubmitStatus::BadMeta
    );
    assert_eq!(
        producer.submit(&[0xff], counted(&released)),
        SubmitStatus::BadMeta
    );
    assert_eq!(
        released.load(Ordering::SeqCst),
        3,
        "refused buffers are released"
    );

    let frame = sink.try_recv().expect("frame");
    assert_eq!(frame.meta, (7, "nv12".to_owned()));
    assert_eq!(frame.buffer.kind(), MemoryKind::Cpu);

    producer.close();
    assert!(producer.is_closed());
    assert_eq!(
        producer.submit(&meta, counted(&released)),
        SubmitStatus::Closed
    );
    assert!(sink.end_reason().is_some_and(|r| r.is_completed()));
}

#[test]
fn a_full_sink_refuses_native_frames() {
    let rt = Runtime::mock().runtime;
    let policy = SinkPolicy::latest().with_overflow(Overflow::DropNewest);
    let sink = rt.open_frame_sink::<u32>(policy).expect("open");
    let producer = rt
        .open_native_producer(sink.id().raw(), 0)
        .expect("producer");
    let meta = codec::encode(&1u32).expect("meta");
    let released = Arc::new(AtomicUsize::new(0));
    assert_eq!(
        producer.submit(&meta, counted(&released)),
        SubmitStatus::Accepted
    );
    assert_eq!(
        producer.submit(&meta, counted(&released)),
        SubmitStatus::Full
    );
    assert_eq!(released.load(Ordering::SeqCst), 1);
    assert_eq!(sink.stats().refused, 1);
}

#[test]
fn closed_ids_are_remembered_up_to_a_bound() {
    let rt = Runtime::mock().runtime;
    let first = rt
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    let first_id = first.id().raw();
    drop(first);
    assert_eq!(
        rt.open_native_producer(first_id, 0).err(),
        Some(SubmitStatus::Closed)
    );
    for _ in 0..300 {
        drop(
            rt.open_frame_sink::<u32>(SinkPolicy::latest())
                .expect("open"),
        );
    }
    assert_eq!(
        rt.open_native_producer(first_id, 0).err(),
        Some(SubmitStatus::UnknownSink),
        "evicted from the recently-closed list"
    );
}

#[test]
fn status_codes_are_stable() {
    for (code, status) in SubmitStatus::ALL.iter().enumerate() {
        assert_eq!(usize::try_from(status.code()).ok(), Some(code));
        assert_eq!(SubmitStatus::from_code(status.code()), Some(*status));
    }
    assert_eq!(SubmitStatus::from_code(-1), None);
    assert_eq!(SubmitStatus::from_code(11), None);
    assert!(SubmitStatus::Displaced.is_taken());
    assert!(SubmitStatus::Closed.should_stop());
    assert!(!SubmitStatus::Full.should_stop());
    assert_eq!(SubmitStatus::TypeMismatch.name(), "TYPE_MISMATCH");
    assert_eq!(NATIVE_ABI_VERSION, 1);
    assert_eq!(fingerprint_of(""), 0xcbf2_9ce4_8422_2325);
}

#[test]
fn panicking_releases_never_reach_the_producer() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    let producer = rt
        .open_native_producer(sink.id().raw(), 0)
        .expect("producer");
    let meta = codec::encode(&1u32).expect("meta");
    let panicky = NativeBuffer::from_bytes(vec![0u8]).with_release(|| panic!("release"));
    assert!(producer.submit(&meta, panicky).is_taken());
    let later = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&later);
    let evicts = catch_unwind(AssertUnwindSafe(|| {
        producer.submit(
            &meta,
            NativeBuffer::from_bytes(vec![1u8])
                .with_release(|| panic!("hook 1"))
                .with_release(move || flag.store(true, Ordering::SeqCst)),
        )
    }));
    assert_eq!(
        evicts.ok(),
        Some(SubmitStatus::Displaced),
        "no unwind out of submit"
    );
    drop(sink);
    assert!(later.load(Ordering::SeqCst), "later hooks still ran");
}

#[test]
fn reentrant_releases_do_not_deadlock() {
    let rt = Runtime::mock().runtime;
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    let id = sink.id();
    let sender = rt.frame_sender(id).expect("sender");
    let hits = Arc::new(AtomicUsize::new(0));
    let (again, rt2, count) = (sender.clone(), Arc::clone(&rt), Arc::clone(&hits));
    let reenter = NativeBuffer::from_bytes(vec![0u8]).with_release(move || {
        count.fetch_add(1, Ordering::SeqCst);
        let _ = again.push(100, empty());
        if let Ok(native) = rt2.open_native_producer(id.raw(), 0) {
            let _ = native.submit(&codec::encode(&101u32).unwrap(), empty());
        }
        let _other = rt2.open_frame_sink::<u8>(SinkPolicy::latest());
    });
    sender.push(1, reenter).expect("push");
    sender.push(2, empty()).expect("push");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    let rt3 = Arc::clone(&rt);
    sender
        .push(
            3,
            empty().with_release(move || {
                if let Ok(native) = rt3.open_native_producer(id.raw(), 0) {
                    native.close();
                }
            }),
        )
        .expect("push");
    drop(sink);
    rt.shutdown();
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the consumer holds the lock while it receives, on purpose: dropping the sink races it"
)]
fn stress_every_buffer_is_released_exactly_once() {
    for round in 0..if cfg!(miri) { 3 } else { 24 } {
        let rt = Runtime::mock().runtime;
        let created = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicUsize::new(0));
        let policy = if round % 2 == 0 {
            SinkPolicy::queue(3)
        } else {
            SinkPolicy::queue(2).with_overflow(Overflow::DropNewest)
        };
        let sink = rt.open_frame_sink::<u32>(policy).expect("open");
        let id = sink.id();
        let sender = rt.frame_sender(id).expect("sender");
        let stop = Arc::new(AtomicBool::new(false));
        let producers: Vec<_> = (0..4u32)
            .map(|p| {
                let (rt, created, released, stop, sender) = (
                    Arc::clone(&rt),
                    Arc::clone(&created),
                    Arc::clone(&released),
                    Arc::clone(&stop),
                    sender.clone(),
                );
                std::thread::spawn(move || {
                    let meta = codec::encode(&p).expect("meta");
                    let native = rt.open_native_producer(id.raw(), 0).ok();
                    while !stop.load(Ordering::Relaxed) {
                        created.fetch_add(1, Ordering::SeqCst);
                        let buffer = counted(&released);
                        match (&native, p % 2) {
                            (Some(native), 1) => drop(native.submit(&meta, buffer)),
                            _ => drop(sender.push(p, buffer)),
                        }
                    }
                })
            })
            .collect();
        drop(sender);
        let sink = Arc::new(Mutex::new(Some(sink)));
        let consumer = {
            let sink = Arc::clone(&sink);
            std::thread::spawn(move || {
                for _ in 0..if cfg!(miri) { 50 } else { 2000 } {
                    let guard = sink.lock().unwrap();
                    let Some(sink) = guard.as_ref() else { break };
                    if let Some(frame) = sink.try_recv() {
                        let clone = frame.buffer.clone();
                        drop(frame);
                        std::thread::spawn(move || drop(clone));
                    }
                }
            })
        };
        std::thread::sleep(Duration::from_millis(5));
        match round % 3 {
            0 => drop(sink.lock().unwrap().take()),
            1 => rt.shutdown(),
            _ => {
                if let Some(sink) = sink.lock().unwrap().as_ref() {
                    sink.close();
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
        stop.store(true, Ordering::Relaxed);
        for producer in producers {
            producer.join().unwrap();
        }
        consumer.join().unwrap();
        drop(sink.lock().unwrap().take());
        rt.shutdown();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            created.load(Ordering::SeqCst),
            released.load(Ordering::SeqCst),
            "round {round}"
        );
    }
}

#[test]
fn remote_plugins_do_not_share_native_frames() {
    let rt = Runtime::mock().runtime;
    assert!(rt.shares_native_frames("local.p"));
    rt.declare_remote_plugin("remote.p");
    assert!(!rt.shares_native_frames("remote.p"));
}

#[test]
fn into_backing_racing_clone_drops_releases_exactly_once() {
    for _ in 0..if cfg!(miri) { 10 } else { 500 } {
        let released = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&released);
        let buffer = NativeBuffer::from_bytes(vec![1u8, 2, 3]).with_release(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        let clone = buffer.clone();
        let dropper = std::thread::spawn(move || drop(clone));
        let taken = buffer.into_backing::<Vec<u8>>();
        dropper.join().unwrap();
        match taken {
            Ok((bytes, lease)) => {
                assert_eq!(bytes, [1, 2, 3]);
                assert_eq!(released.load(Ordering::SeqCst), 0);
                drop(lease);
            }
            Err(err) => drop(err.into_buffer()),
        }
        assert_eq!(released.load(Ordering::SeqCst), 1);
    }
}
