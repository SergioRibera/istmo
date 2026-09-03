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
