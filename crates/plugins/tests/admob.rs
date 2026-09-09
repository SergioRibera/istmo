//! `AdMob` plugin: `acquire_with` + load/show round-trip + banner handle
//! cascade against a mock native backend.

use std::sync::Arc;
use std::thread;

use istmo_core::{Envelope, Frame, InstanceId, NativeHandleId, Runtime, codec};
use istmo_plugins::{
    ADMOB_PLUGIN_ID, AdError, AdMobClient, AdMobConfig, BannerRect, BannerRequest,
    InterstitialOutcome, RewardedOutcome,
};

fn cfg() -> AdMobConfig {
    AdMobConfig {
        app_id: "ca-app-pub-3940256099942544~3347511713".to_owned(),
        test_device_ids: vec!["TEST-DEV-01".to_owned()],
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
        Frame::CreateInstance {
            call_id,
            plugin_id,
            payload,
        } => {
            assert_eq!(plugin_id, ADMOB_PLUGIN_ID);
            let (decoded, _) = codec::decode::<AdMobConfig>(&payload).unwrap();
            assert_eq!(decoded, cfg());
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

#[test]
fn load_and_show_interstitial_round_trip() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &outbound, InstanceId(1));

        // load_interstitial -> returns handle id 7
        let env = outbound.recv().expect("load");
        let (load_call, ad_unit) = match env.frame {
            Frame::Call {
                call_id,
                method,
                payload,
                ..
            } => {
                assert_eq!(method, "load_interstitial");
                let ((unit,), _) = codec::decode::<(String,)>(&payload).unwrap();
                (call_id, unit)
            }
            other => panic!("expected Call load, got {other:?}"),
        };
        assert_eq!(ad_unit, "ca-app-pub-.../interstitial");
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: load_call,
                result: Ok(codec::encode(&NativeHandleId(7)).unwrap()),
            }))
            .unwrap();

        // show_interstitial -> returns Dismissed
        let env = outbound.recv().expect("show");
        let show_call = match env.frame {
            Frame::Call {
                call_id,
                method,
                payload,
                ..
            } => {
                assert_eq!(method, "show_interstitial");
                let ((handle,), _) = codec::decode::<(NativeHandleId,)>(&payload).unwrap();
                assert_eq!(handle, NativeHandleId(7));
                call_id
            }
            other => panic!("expected Call show, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: show_call,
                result: Ok(codec::encode(&InterstitialOutcome::Dismissed).unwrap()),
            }))
            .unwrap();
    });

    let client = pollster::block_on(AdMobClient::from_runtime_with(&rt, cfg())).unwrap();
    let ad = pollster::block_on(
        client.load_interstitial_owned("ca-app-pub-.../interstitial".to_owned()),
    )
    .expect("load");
    assert_eq!(ad.id(), NativeHandleId(7));
    let outcome = pollster::block_on(client.show_interstitial_owned(ad)).expect("show");
    assert_eq!(outcome, InterstitialOutcome::Dismissed);
    backend.join().unwrap();
}

#[test]
fn rewarded_ad_returns_reward_payload() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &outbound, InstanceId(1));

        let env = outbound.recv().expect("load");
        let load_call = match env.frame {
            Frame::Call { call_id, .. } => call_id,
            other => panic!("expected Call, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: load_call,
                result: Ok(codec::encode(&NativeHandleId(11)).unwrap()),
            }))
            .unwrap();

        let env = outbound.recv().expect("show");
        let show_call = match env.frame {
            Frame::Call { call_id, .. } => call_id,
            other => panic!("expected Call, got {other:?}"),
        };
        let reward = RewardedOutcome {
            granted: true,
            reward_type: "coins".to_owned(),
            reward_amount: 25,
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: show_call,
                result: Ok(codec::encode(&reward).unwrap()),
            }))
            .unwrap();
    });

    let client = pollster::block_on(AdMobClient::from_runtime_with(&rt, cfg())).unwrap();
    let ad = pollster::block_on(client.load_rewarded_owned("ca-app-pub-.../rewarded".to_owned()))
        .unwrap();
    let outcome = pollster::block_on(client.show_rewarded_owned(ad)).unwrap();
    assert!(outcome.granted);
    assert_eq!(outcome.reward_type, "coins");
    assert_eq!(outcome.reward_amount, 25);
    backend.join().unwrap();
}

#[test]
fn dropping_banner_handle_emits_release_frame() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend_outbound = outbound.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &backend_outbound, InstanceId(1));

        let env = backend_outbound.recv().expect("show_banner");
        let call = match env.frame {
            Frame::Call {
                call_id,
                method,
                payload,
                ..
            } => {
                assert_eq!(method, "show_banner");
                let ((req,), _) = codec::decode::<(BannerRequest,)>(&payload).unwrap();
                assert_eq!(req.rect.width, 320);
                call_id
            }
            other => panic!("expected Call, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: call,
                result: Ok(codec::encode(&NativeHandleId(88)).unwrap()),
            }))
            .unwrap();
        drop(backend_outbound);
    });

    let client = pollster::block_on(AdMobClient::from_runtime_with(&rt, cfg())).unwrap();
    let banner = pollster::block_on(client.show_banner_owned(BannerRequest {
        ad_unit_id: "ca-app-pub-.../banner".to_owned(),
        rect: BannerRect {
            x: 0,
            y: 800,
            width: 320,
            height: 50,
        },
    }))
    .unwrap();
    backend.join().unwrap();
    assert_eq!(banner.id(), NativeHandleId(88));

    drop(banner);
    let release = outbound.recv().expect("release");
    match release.frame {
        Frame::ReleaseNativeHandle { handle_id } => {
            assert_eq!(handle_id, NativeHandleId(88));
        }
        other => panic!("expected ReleaseNativeHandle, got {other:?}"),
    }
}

#[test]
fn update_banner_forwards_new_rect_via_typed_call() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend_outbound = outbound.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &backend_outbound, InstanceId(1));

        // show_banner → returns id 42
        let env = backend_outbound.recv().expect("show_banner");
        let show_call = match env.frame {
            Frame::Call {
                call_id, method, ..
            } => {
                assert_eq!(method, "show_banner");
                call_id
            }
            other => panic!("expected show Call, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: show_call,
                result: Ok(codec::encode(&NativeHandleId(42)).unwrap()),
            }))
            .unwrap();

        // update_banner → verify new rect, respond ok
        let env = backend_outbound.recv().expect("update_banner");
        let (call, handle_arg, rect_arg) = match env.frame {
            Frame::Call {
                call_id,
                method,
                payload,
                ..
            } => {
                assert_eq!(method, "update_banner");
                let ((handle, rect), _) =
                    codec::decode::<(NativeHandleId, BannerRect)>(&payload).unwrap();
                (call_id, handle, rect)
            }
            other => panic!("expected update Call, got {other:?}"),
        };
        assert_eq!(handle_arg, NativeHandleId(42));
        assert_eq!(rect_arg.x, 12);
        assert_eq!(rect_arg.y, 400);
        assert_eq!(rect_arg.width, 320);
        assert_eq!(rect_arg.height, 100);
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: call,
                result: Ok(vec![]),
            }))
            .unwrap();
        drop(backend_outbound);
    });

    let client = pollster::block_on(AdMobClient::from_runtime_with(&rt, cfg())).unwrap();
    let banner = pollster::block_on(client.show_banner_owned(BannerRequest {
        ad_unit_id: "unit".to_owned(),
        rect: BannerRect {
            x: 0,
            y: 0,
            width: 320,
            height: 50,
        },
    }))
    .unwrap();
    pollster::block_on(client.update_banner_owned(
        &banner,
        BannerRect {
            x: 12,
            y: 400,
            width: 320,
            height: 100,
        },
    ))
    .expect("update");
    backend.join().unwrap();
    // Handle still alive → dropping it now still triggers release. Prove it.
    drop(banner);
    let release = outbound.recv().expect("release after update+drop");
    assert!(
        matches!(release.frame, Frame::ReleaseNativeHandle { handle_id } if handle_id == NativeHandleId(42))
    );
}

#[test]
fn hide_banner_consumes_handle_and_suppresses_release_frame() {
    // `hide_banner_owned` calls `into_id()` — the handle disappears
    // without ever firing a `ReleaseNativeHandle`. Native side is
    // responsible for releasing its own registry entry inside the hide
    // handler.
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend_outbound = outbound.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &backend_outbound, InstanceId(1));

        let env = backend_outbound.recv().expect("show_banner");
        let show_call = match env.frame {
            Frame::Call { call_id, .. } => call_id,
            other => panic!("expected show Call, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: show_call,
                result: Ok(codec::encode(&NativeHandleId(99)).unwrap()),
            }))
            .unwrap();

        let env = backend_outbound.recv().expect("hide_banner");
        let (hide_call, handle_arg) = match env.frame {
            Frame::Call {
                call_id,
                method,
                payload,
                ..
            } => {
                assert_eq!(method, "hide_banner");
                let ((handle,), _) = codec::decode::<(NativeHandleId,)>(&payload).unwrap();
                (call_id, handle)
            }
            other => panic!("expected hide Call, got {other:?}"),
        };
        assert_eq!(handle_arg, NativeHandleId(99));
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: hide_call,
                result: Ok(vec![]),
            }))
            .unwrap();
        drop(backend_outbound);
    });

    let client = pollster::block_on(AdMobClient::from_runtime_with(&rt, cfg())).unwrap();
    let banner = pollster::block_on(client.show_banner_owned(BannerRequest {
        ad_unit_id: "unit".to_owned(),
        rect: BannerRect {
            x: 0,
            y: 800,
            width: 320,
            height: 50,
        },
    }))
    .unwrap();
    pollster::block_on(client.hide_banner_owned(banner)).expect("hide");
    backend.join().unwrap();

    // No further outbound frames — hide consumed the handle, no
    // ReleaseNativeHandle should appear.
    assert!(
        outbound.try_recv().is_err(),
        "unexpected outbound after hide"
    );
}

#[test]
fn rewarded_reports_not_granted_when_user_dismisses_early() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &outbound, InstanceId(1));

        let load = outbound.recv().unwrap();
        let load_call = match load.frame {
            Frame::Call { call_id, .. } => call_id,
            other => panic!("expected load, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: load_call,
                result: Ok(codec::encode(&NativeHandleId(3)).unwrap()),
            }))
            .unwrap();

        let show = outbound.recv().unwrap();
        let show_call = match show.frame {
            Frame::Call { call_id, .. } => call_id,
            other => panic!("expected show, got {other:?}"),
        };
        let outcome = RewardedOutcome {
            granted: false,
            reward_type: String::new(),
            reward_amount: 0,
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: show_call,
                result: Ok(codec::encode(&outcome).unwrap()),
            }))
            .unwrap();
    });

    let client = pollster::block_on(AdMobClient::from_runtime_with(&rt, cfg())).unwrap();
    let ad = pollster::block_on(client.load_rewarded_owned("unit".to_owned())).unwrap();
    let outcome = pollster::block_on(client.show_rewarded_owned(ad)).unwrap();
    assert!(!outcome.granted, "user dismissed → no reward");
    assert_eq!(outcome.reward_amount, 0);
    assert!(outcome.reward_type.is_empty());
    backend.join().unwrap();
}

#[test]
fn interstitial_failed_to_show_is_a_valid_terminal_outcome() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &outbound, InstanceId(1));

        let load = outbound.recv().unwrap();
        let load_call = match load.frame {
            Frame::Call { call_id, .. } => call_id,
            other => panic!("expected load, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: load_call,
                result: Ok(codec::encode(&NativeHandleId(5)).unwrap()),
            }))
            .unwrap();

        let show = outbound.recv().unwrap();
        let show_call = match show.frame {
            Frame::Call { call_id, .. } => call_id,
            other => panic!("expected show, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: show_call,
                result: Ok(codec::encode(&InterstitialOutcome::FailedToShow).unwrap()),
            }))
            .unwrap();
    });

    let client = pollster::block_on(AdMobClient::from_runtime_with(&rt, cfg())).unwrap();
    let ad = pollster::block_on(client.load_interstitial_owned("unit".to_owned())).unwrap();
    let outcome = pollster::block_on(client.show_interstitial_owned(ad)).unwrap();
    assert_eq!(outcome, InterstitialOutcome::FailedToShow);
    backend.join().unwrap();
}

#[test]
fn every_ad_error_variant_round_trips_through_the_wire() {
    // Exhaustive check that AdError's typed variants survive the
    // encode/decode boundary — a silent drift here would surface as
    // "undecodable domain error" on the demo side.
    let variants = [
        AdError::NotInitialized,
        AdError::NoFill,
        AdError::Network("timeout".to_owned()),
        AdError::InvalidRequest("empty ad unit id".to_owned()),
        AdError::UnknownAd,
        AdError::Internal("sdk boom".to_owned()),
    ];
    for original in variants {
        let bytes = codec::encode(&original).expect("encode");
        let (decoded, _) = codec::decode::<AdError>(&bytes).expect("decode");
        assert_eq!(decoded, original);
    }
}

#[test]
fn config_with_child_directed_treatment_round_trips_intact() {
    let cfg = AdMobConfig {
        app_id: "ca-app-pub-xxxx~yyyy".to_owned(),
        test_device_ids: vec!["A".to_owned(), "B".to_owned(), "C".to_owned()],
        child_directed_treatment: true,
    };
    let bytes = codec::encode(&cfg).unwrap();
    let (decoded, _) = codec::decode::<AdMobConfig>(&bytes).unwrap();
    assert_eq!(decoded, cfg);
    assert!(decoded.child_directed_treatment);
    assert_eq!(decoded.test_device_ids.len(), 3);
}

#[test]
fn distinct_loads_produce_distinct_native_handles() {
    // The client itself is stateless — each load call is independent.
    // Verify by handing back two different ids and asserting the
    // wrapper handles do not collide.
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound.clone();

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &outbound, InstanceId(1));
        for id in [111u64, 222u64] {
            let env = outbound.recv().expect("load");
            let call = match env.frame {
                Frame::Call { call_id, .. } => call_id,
                other => panic!("expected Call, got {other:?}"),
            };
            backend_rt
                .dispatch_inbound(Envelope::new(Frame::Respond {
                    call_id: call,
                    result: Ok(codec::encode(&NativeHandleId(id)).unwrap()),
                }))
                .unwrap();
        }
    });

    let client = pollster::block_on(AdMobClient::from_runtime_with(&rt, cfg())).unwrap();
    let a = pollster::block_on(client.load_interstitial_owned("unit-a".to_owned())).unwrap();
    let b = pollster::block_on(client.load_interstitial_owned("unit-b".to_owned())).unwrap();
    assert_ne!(a.id(), b.id());
    assert_eq!(a.id(), NativeHandleId(111));
    assert_eq!(b.id(), NativeHandleId(222));
    backend.join().unwrap();

    // Drain the two release frames the drops will emit — order matches
    // drop order (a first, then b).
    drop(a);
    drop(b);
    let mut released: Vec<u64> = Vec::new();
    while let Ok(env) = init.outbound.try_recv() {
        if let Frame::ReleaseNativeHandle { handle_id } = env.frame {
            released.push(handle_id.0);
        }
    }
    assert_eq!(released, vec![111, 222]);
}

#[test]
fn load_no_fill_surfaces_as_typed_error() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        spawn_create_instance(&backend_rt, &outbound, InstanceId(1));

        let env = outbound.recv().expect("load");
        let call = match env.frame {
            Frame::Call { call_id, .. } => call_id,
            other => panic!("expected Call, got {other:?}"),
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id: call,
                result: Err(codec::encode(&AdError::NoFill).unwrap()),
            }))
            .unwrap();
    });

    let client = pollster::block_on(AdMobClient::from_runtime_with(&rt, cfg())).unwrap();
    let err =
        pollster::block_on(client.load_interstitial("ca-app-pub-.../interstitial".to_owned()))
            .expect_err("no fill");
    let bytes = match err {
        istmo_core::IstmoError::PluginError { bytes } => bytes,
        other => panic!("expected PluginError, got {other:?}"),
    };
    let (decoded, _) = codec::decode::<AdError>(&bytes).unwrap();
    assert_eq!(decoded, AdError::NoFill);
    backend.join().unwrap();
}
