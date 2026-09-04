//! `BannerSlot` state-machine + sync loop against a mock native backend.
//!
//! The slot is UI-agnostic — nothing here mentions egui or any other
//! framework. Tests inject a synchronous spawn (foreground executor) so
//! we can assert wire frames deterministically without a runtime.

// Cloning `flume::Receiver` and `Arc<Runtime>` in tests is intentional
// — one clone lands in the backend thread, one stays in the assertion
// scope. Clippy sees redundant cases where the original is not used
// after the clone; readability wins over the micro-optimisation here.
#![allow(clippy::redundant_clone)]

use std::sync::{Arc, Mutex};
use std::thread;

use istmo_core::{Envelope, Frame, InstanceId, NativeHandleId, Runtime, codec};
use istmo_plugins::{
    ADMOB_PLUGIN_ID, AdMobClient, AdMobConfig, BannerRect, BannerSlot, SlotStatus, SlotTarget,
    banner_rect_from_logical,
};

fn cfg() -> AdMobConfig {
    AdMobConfig {
        app_id: "ca-app-pub-3940256099942544~3347511713".to_owned(),
        test_device_ids: Vec::new(),
        child_directed_treatment: false,
    }
}

fn spawn_create_instance(
    rt: &Arc<Runtime>,
    outbound: &flume::Receiver<Envelope>,
    instance_id: InstanceId,
) {
    let env = outbound.recv().expect("create envelope");
    let call_id = match env.frame {
        Frame::CreateInstance { call_id, plugin_id, .. } => {
            assert_eq!(plugin_id, ADMOB_PLUGIN_ID);
            call_id
        }
        other => panic!("expected CreateInstance, got {other:?}"),
    };
    rt.dispatch_inbound(Envelope::new(Frame::Respond {
        call_id,
        result: Ok(codec::encode(&instance_id).unwrap()),
    }))
    .unwrap();
}

/// Sync executor: runs futures on the current thread via pollster,
/// making test flow deterministic. Real slots use OS threads.
fn sync_spawn() -> impl Fn(istmo_plugins::BannerSlotBoxFuture) + Send + Sync + 'static {
    |fut| pollster::block_on(fut)
}

fn build_client(rt: &Arc<Runtime>) -> Arc<AdMobClient> {
    Arc::new(pollster::block_on(AdMobClient::from_runtime_with(rt, cfg())).unwrap())
}

#[test]
fn slot_starts_idle_and_reports_no_error() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound.clone();

    // Answer the CreateInstance so from_runtime_with can complete.
    let backend_rt = rt.clone();
    let backend = thread::spawn(move || spawn_create_instance(&backend_rt, &outbound, InstanceId(1)));
    let client = build_client(&rt);
    backend.join().unwrap();

    let slot = BannerSlot::new("ad-unit", client).with_spawn(sync_spawn());
    assert_eq!(slot.status(), SlotStatus::Idle);
    assert!(slot.last_error().is_none());
    assert!(!slot.is_shown());
}

#[test]
fn show_triggers_show_banner_call_and_transitions_to_live() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound.clone();

    let backend_rt = rt.clone();
    let backend_outbound = outbound.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &backend_outbound, InstanceId(1));
        // Answer show_banner with handle 42
        let env = backend_outbound.recv().expect("show");
        let (call_id, req) = match env.frame {
            Frame::Call { call_id, method, payload, .. } => {
                assert_eq!(method, "show_banner");
                let ((r,), _) =
                    codec::decode::<(istmo_plugins::BannerRequest,)>(&payload).unwrap();
                (call_id, r)
            }
            other => panic!("expected show Call, got {other:?}"),
        };
        assert_eq!(req.ad_unit_id, "ad-unit");
        assert_eq!(req.rect, BannerRect { x: 10, y: 20, width: 320, height: 50 });
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Ok(codec::encode(&NativeHandleId(42)).unwrap()),
            }))
            .unwrap();
    });

    let client = build_client(&rt);
    let slot = BannerSlot::new("ad-unit", client).with_spawn(sync_spawn());
    slot.sync(SlotTarget::Show(BannerRect { x: 10, y: 20, width: 320, height: 50 }));
    backend.join().unwrap();

    assert_eq!(slot.status(), SlotStatus::Live);
    assert!(slot.is_shown());
    assert!(slot.last_error().is_none());
}

#[test]
fn syncing_the_same_rect_twice_produces_a_single_show_call() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound.clone();

    let backend_rt = rt.clone();
    let backend_outbound = outbound.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &backend_outbound, InstanceId(1));
        // Only one show call expected.
        let env = backend_outbound.recv().unwrap();
        let call_id = match env.frame {
            Frame::Call { call_id, .. } => call_id,
            other => panic!("expected Call, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Ok(codec::encode(&NativeHandleId(1)).unwrap()),
            }))
            .unwrap();
    });

    let client = build_client(&rt);
    let slot = BannerSlot::new("u", client).with_spawn(sync_spawn());
    let rect = BannerRect { x: 0, y: 0, width: 320, height: 50 };
    slot.sync(SlotTarget::Show(rect));
    slot.sync(SlotTarget::Show(rect));
    slot.sync(SlotTarget::Show(rect));
    backend.join().unwrap();

    // No further outbound frames.
    assert!(outbound.try_recv().is_err(), "expected no follow-up call, got one");
}

#[test]
fn changing_rect_after_live_triggers_update_banner_without_reload() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound.clone();

    let backend_rt = rt.clone();
    let backend_outbound = outbound.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &backend_outbound, InstanceId(1));

        // show_banner
        let env = backend_outbound.recv().unwrap();
        let show_call = match env.frame {
            Frame::Call { call_id, method, .. } => {
                assert_eq!(method, "show_banner");
                call_id
            }
            other => panic!("expected show, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: show_call,
                result: Ok(codec::encode(&NativeHandleId(7)).unwrap()),
            }))
            .unwrap();

        // update_banner
        let env = backend_outbound.recv().unwrap();
        let (call_id, handle_arg, rect_arg) = match env.frame {
            Frame::Call { call_id, method, payload, .. } => {
                assert_eq!(method, "update_banner");
                let ((h, r), _) = codec::decode::<(NativeHandleId, BannerRect)>(&payload).unwrap();
                (call_id, h, r)
            }
            other => panic!("expected update, got {other:?}"),
        };
        assert_eq!(handle_arg, NativeHandleId(7));
        assert_eq!(rect_arg, BannerRect { x: 100, y: 200, width: 320, height: 50 });
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Ok(vec![]),
            }))
            .unwrap();
    });

    let client = build_client(&rt);
    let slot = BannerSlot::new("u", client).with_spawn(sync_spawn());
    slot.sync(SlotTarget::Show(BannerRect { x: 0, y: 0, width: 320, height: 50 }));
    slot.sync(SlotTarget::Show(BannerRect { x: 100, y: 200, width: 320, height: 50 }));
    backend.join().unwrap();

    // Still Live — update did not tear down.
    assert_eq!(slot.status(), SlotStatus::Live);
    assert!(outbound.try_recv().is_err(), "no extra frames expected");
}

#[test]
fn hide_after_live_calls_hide_banner_and_returns_to_idle() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound.clone();

    let backend_rt = rt.clone();
    let backend_outbound = outbound.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &backend_outbound, InstanceId(1));

        let env = backend_outbound.recv().unwrap();
        let show_call = match env.frame {
            Frame::Call { call_id, .. } => call_id,
            other => panic!("expected show, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: show_call,
                result: Ok(codec::encode(&NativeHandleId(99)).unwrap()),
            }))
            .unwrap();

        let env = backend_outbound.recv().unwrap();
        let (hide_call, handle_arg) = match env.frame {
            Frame::Call { call_id, method, payload, .. } => {
                assert_eq!(method, "hide_banner");
                let ((h,), _) = codec::decode::<(NativeHandleId,)>(&payload).unwrap();
                (call_id, h)
            }
            other => panic!("expected hide, got {other:?}"),
        };
        assert_eq!(handle_arg, NativeHandleId(99));
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: hide_call,
                result: Ok(vec![]),
            }))
            .unwrap();
    });

    let client = build_client(&rt);
    let slot = BannerSlot::new("u", client).with_spawn(sync_spawn());
    slot.sync(SlotTarget::Show(BannerRect { x: 0, y: 0, width: 320, height: 50 }));
    assert_eq!(slot.status(), SlotStatus::Live);
    slot.sync(SlotTarget::Hide);
    backend.join().unwrap();

    assert_eq!(slot.status(), SlotStatus::Idle);
    // hide_banner_owned uses into_id — no ReleaseNativeHandle should
    // appear on the wire.
    assert!(outbound.try_recv().is_err(), "hide should suppress release");
}

#[test]
fn hide_while_idle_is_a_noop() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound.clone();

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || spawn_create_instance(&backend_rt, &outbound, InstanceId(1)));
    let client = build_client(&rt);
    backend.join().unwrap();

    let slot = BannerSlot::new("u", client).with_spawn(sync_spawn());
    slot.sync(SlotTarget::Hide);
    slot.sync(SlotTarget::Hide);
    assert_eq!(slot.status(), SlotStatus::Idle);
    assert!(init.outbound.try_recv().is_err(), "no frames for Hide/Idle");
}

#[test]
fn show_domain_error_records_typed_ad_error() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound.clone();

    let backend_rt = rt.clone();
    let backend_outbound = outbound.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &backend_outbound, InstanceId(1));

        let env = backend_outbound.recv().unwrap();
        let call = match env.frame {
            Frame::Call { call_id, .. } => call_id,
            other => panic!("expected Call, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: call,
                result: Err(codec::encode(&istmo_plugins::AdError::NoFill).unwrap()),
            }))
            .unwrap();
    });

    let client = build_client(&rt);
    let slot = BannerSlot::new("u", client).with_spawn(sync_spawn());
    slot.sync(SlotTarget::Show(BannerRect { x: 0, y: 0, width: 320, height: 50 }));
    backend.join().unwrap();

    assert_eq!(slot.status(), SlotStatus::Idle, "failed show → back to Idle");
    assert_eq!(slot.last_error(), Some(istmo_plugins::AdError::NoFill));
}

#[test]
fn rect_helper_converts_logical_units_via_scale_and_clamps_negatives() {
    let r = banner_rect_from_logical(10.0, 20.0, 320.0, 50.0, 2.5);
    assert_eq!(r, BannerRect { x: 25, y: 50, width: 800, height: 125 });

    // Negative coordinates clamp to zero — a UI computing a rect
    // partially off the top of the screen should not send negative
    // pixel values.
    let clipped = banner_rect_from_logical(-4.0, -10.0, 100.0, 50.0, 3.0);
    assert_eq!(clipped, BannerRect { x: 0, y: 0, width: 300, height: 150 });
}

#[test]
fn multiple_slots_can_share_the_same_client_arc() {
    // Two slots against the same AdMobClient — both operate independently
    // over the wire but reuse the underlying plugin instance.
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound.clone();

    let backend_rt = rt.clone();
    let backend_outbound = outbound.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &backend_outbound, InstanceId(1));
        for expected_id in [10u64, 20u64] {
            let env = backend_outbound.recv().unwrap();
            let call = match env.frame {
                Frame::Call { call_id, .. } => call_id,
                other => panic!("expected Call, got {other:?}"),
            };
            backend_rt
                .dispatch_inbound(Envelope::new(Frame::Respond {
                    call_id: call,
                    result: Ok(codec::encode(&NativeHandleId(expected_id)).unwrap()),
                }))
                .unwrap();
        }
    });

    let client = build_client(&rt);
    let slot_a = BannerSlot::new("a", client.clone()).with_spawn(sync_spawn());
    let slot_b = BannerSlot::new("b", client).with_spawn(sync_spawn());
    slot_a.sync(SlotTarget::Show(BannerRect { x: 0, y: 0, width: 320, height: 50 }));
    slot_b.sync(SlotTarget::Show(BannerRect { x: 0, y: 800, width: 320, height: 50 }));
    backend.join().unwrap();

    assert_eq!(slot_a.status(), SlotStatus::Live);
    assert_eq!(slot_b.status(), SlotStatus::Live);
}

#[test]
fn rapid_rect_changes_coalesce_to_the_latest_via_one_updater_task() {
    // Simulate a fast scroll: sync() fires four different rects in a
    // row while the first update_banner is in flight. The coalescing
    // updater loop should:
    //   * spawn exactly ONE update task
    //   * skip the intermediate rects
    //   * end with the LATEST rect on the wire
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound.clone();

    // Block the update handler until we've signalled it via a channel.
    // While it's blocked, we shove more rects into the slot; when
    // released it drains only the latest.
    let (release_tx, release_rx) = flume::bounded::<()>(1);

    let backend_rt = rt.clone();
    let backend_outbound = outbound.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &backend_outbound, InstanceId(1));

        // Show
        let env = backend_outbound.recv().unwrap();
        let show_call = match env.frame {
            Frame::Call { call_id, .. } => call_id,
            other => panic!("expected show, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: show_call,
                result: Ok(codec::encode(&NativeHandleId(1)).unwrap()),
            }))
            .unwrap();

        // First update — do NOT respond until release_rx fires.
        let first = backend_outbound.recv().unwrap();
        let first_call = match &first.frame {
            Frame::Call { call_id, method, payload, .. } => {
                assert_eq!(method, "update_banner");
                let ((_, r), _) = codec::decode::<(NativeHandleId, BannerRect)>(payload).unwrap();
                assert_eq!(r.x, 10, "first update rect");
                *call_id
            }
            other => panic!("expected first update, got {other:?}"),
        };
        // Hold the caller until the test has queued more rects.
        release_rx.recv().unwrap();
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: first_call,
                result: Ok(vec![]),
            }))
            .unwrap();

        // The coalesced next call should carry the LATEST rect only.
        let next = backend_outbound.recv().unwrap();
        match next.frame {
            Frame::Call { method, payload, call_id, .. } => {
                assert_eq!(method, "update_banner");
                let ((_, r), _) = codec::decode::<(NativeHandleId, BannerRect)>(&payload).unwrap();
                assert_eq!(r.x, 40, "coalesced update should carry latest rect only");
                backend_rt
                    .dispatch_inbound(Envelope::new(Frame::Respond {
                        call_id,
                        result: Ok(vec![]),
                    }))
                    .unwrap();
            }
            other => panic!("expected coalesced update, got {other:?}"),
        }
    });

    let client = build_client(&rt);
    // Use a REAL thread-per-future spawn so the update task actually
    // parks on the mutex when the backend delays its response. The
    // `sync_spawn` foreground executor would deadlock.
    let slot = BannerSlot::new("u", client);
    slot.sync(SlotTarget::Show(BannerRect { x: 0, y: 0, width: 320, height: 50 }));
    // Poll until Live — the backend answers show_banner immediately.
    while slot.status() != SlotStatus::Live {
        thread::sleep(std::time::Duration::from_millis(2));
    }

    // Fire the first update; the backend blocks on it until release_tx.
    slot.sync(SlotTarget::Show(BannerRect { x: 10, y: 0, width: 320, height: 50 }));
    // Wait for the first update Call to leave the outbound.
    thread::sleep(std::time::Duration::from_millis(50));

    // Now spam intermediate rects — they should coalesce into the last.
    slot.sync(SlotTarget::Show(BannerRect { x: 20, y: 0, width: 320, height: 50 }));
    slot.sync(SlotTarget::Show(BannerRect { x: 30, y: 0, width: 320, height: 50 }));
    slot.sync(SlotTarget::Show(BannerRect { x: 40, y: 0, width: 320, height: 50 }));

    // Release the backend — it will send its second update, which must
    // carry rect x=40 (the newest), never x=20 or x=30.
    release_tx.send(()).unwrap();
    backend.join().unwrap();

    assert_eq!(slot.status(), SlotStatus::Live);
}

#[test]
fn injected_spawn_is_the_only_executor_used() {
    // Prove the spawn abstraction actually receives every scheduled
    // future — a bug where the slot cheated and spawned an OS thread
    // instead would slip past the other tests.
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound.clone();

    let backend_rt = rt.clone();
    let backend_outbound = outbound.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &backend_outbound, InstanceId(1));

        let env = backend_outbound.recv().unwrap();
        let call = match env.frame {
            Frame::Call { call_id, .. } => call_id,
            other => panic!("expected Call, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: call,
                result: Ok(codec::encode(&NativeHandleId(5)).unwrap()),
            }))
            .unwrap();
    });

    let client = build_client(&rt);
    let counter = Arc::new(Mutex::new(0u32));
    let counter_clone = counter.clone();
    let slot = BannerSlot::new("u", client).with_spawn(move |fut| {
        *counter_clone.lock().unwrap() += 1;
        pollster::block_on(fut);
    });
    slot.sync(SlotTarget::Show(BannerRect { x: 0, y: 0, width: 320, height: 50 }));
    backend.join().unwrap();

    assert_eq!(*counter.lock().unwrap(), 1, "exactly one spawn per action");
}
