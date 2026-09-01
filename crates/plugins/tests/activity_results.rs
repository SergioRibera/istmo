//! ActivityResults plugin: round-trips success and typed launch errors.

use std::thread;

use istmo_core::{Envelope, Frame, Runtime, codec};
use istmo_plugins::{
    ACTIVITY_RESULTS_PLUGIN_ID, ActivityLaunchError, ActivityOutcome, ActivityResult,
    ActivityResults, ExtraValue, IntentRequest,
};

fn sample_request() -> IntentRequest {
    IntentRequest {
        action: "android.intent.action.VIEW".to_owned(),
        uri: Some("https://example.com".to_owned()),
        component: None,
        mime_type: None,
        categories: vec![],
        extras: vec![("tab".to_owned(), ExtraValue::Int(3))],
    }
}

#[test]
fn launch_round_trips_ok_result() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        let env = outbound.recv().expect("call");
        let Frame::Call {
            call_id,
            plugin_id,
            method,
            payload,
            ..
        } = env.frame
        else {
            panic!("expected call");
        };
        assert_eq!(plugin_id, ACTIVITY_RESULTS_PLUGIN_ID);
        assert_eq!(method, "launch");
        let ((decoded,), _) = codec::decode::<(IntentRequest,)>(&payload).unwrap();
        assert_eq!(decoded, sample_request());
        let result = ActivityResult {
            outcome: ActivityOutcome::Ok,
            data_uri: Some("content://result".to_owned()),
            extras: vec![("tab".to_owned(), ExtraValue::Text("done".to_owned()))],
        };
        let bytes = codec::encode(&result).unwrap();
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Ok(bytes),
            }))
            .unwrap();
    });

    let plugin = ActivityResults::from_runtime(&rt).expect("declared");
    let result = pollster::block_on(plugin.launch(sample_request())).unwrap();
    assert_eq!(result.outcome, ActivityOutcome::Ok);
    assert_eq!(result.data_uri.as_deref(), Some("content://result"));
    backend.join().unwrap();
}

#[test]
fn launch_surfaces_cancelled_outcome_without_erroring() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        let env = outbound.recv().expect("call");
        let Frame::Call { call_id, .. } = env.frame else {
            panic!("expected call");
        };
        let result = ActivityResult {
            outcome: ActivityOutcome::Cancelled,
            data_uri: None,
            extras: vec![],
        };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Ok(codec::encode(&result).unwrap()),
            }))
            .unwrap();
    });

    let plugin = ActivityResults::from_runtime(&rt).expect("declared");
    let result = pollster::block_on(plugin.launch(sample_request())).unwrap();
    assert_eq!(result.outcome, ActivityOutcome::Cancelled);
    backend.join().unwrap();
}

#[test]
fn launch_maps_domain_error_to_plugin_error_bytes() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        let env = outbound.recv().expect("call");
        let Frame::Call { call_id, .. } = env.frame else {
            panic!("expected call");
        };
        let err = ActivityLaunchError::NoActivityFound;
        let bytes = codec::encode(&err).unwrap();
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Err(bytes),
            }))
            .unwrap();
    });

    let plugin = ActivityResults::from_runtime(&rt).expect("declared");
    let err = pollster::block_on(plugin.launch(sample_request())).expect_err("should fail");
    let bytes = match err {
        istmo_core::IstmoError::PluginError { bytes } => bytes,
        other => panic!("expected PluginError, got {other:?}"),
    };
    let (decoded, _) = codec::decode::<ActivityLaunchError>(&bytes).unwrap();
    assert_eq!(decoded, ActivityLaunchError::NoActivityFound);
    backend.join().unwrap();
}
