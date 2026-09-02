//! Permissions plugin: end-to-end round-trip against a mock backend.

use std::thread;

use istmo_core::{Envelope, Frame, Runtime, codec};
use istmo_plugins::{PERMISSIONS_PLUGIN_ID, PermissionOutcome, PermissionStatus, PermissionsClient};

#[test]
fn check_round_trips_status() {
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
        assert_eq!(plugin_id, PERMISSIONS_PLUGIN_ID);
        assert_eq!(method, "check");
        let ((permission,), _) = codec::decode::<(String,)>(&payload).unwrap();
        assert_eq!(permission, "android.permission.CAMERA");
        let bytes = codec::encode(&PermissionStatus::Granted).unwrap();
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Ok(bytes),
            }))
            .unwrap();
    });

    let plugin = PermissionsClient::from_runtime(&rt).expect("declared");
    let status = pollster::block_on(plugin.check("android.permission.CAMERA".to_owned())).unwrap();
    assert_eq!(status, PermissionStatus::Granted);
    backend.join().unwrap();
}

#[test]
fn request_returns_outcome_per_input() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        let env = outbound.recv().expect("call");
        let Frame::Call {
            call_id,
            method,
            payload,
            ..
        } = env.frame
        else {
            panic!("expected call");
        };
        assert_eq!(method, "request");
        let ((permissions,), _) = codec::decode::<(Vec<String>,)>(&payload).unwrap();
        let outcomes: Vec<PermissionOutcome> = permissions
            .into_iter()
            .map(|permission| PermissionOutcome {
                permission,
                status: PermissionStatus::Granted,
            })
            .collect();
        let bytes = codec::encode(&outcomes).unwrap();
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Ok(bytes),
            }))
            .unwrap();
    });

    let plugin = PermissionsClient::from_runtime(&rt).expect("declared");
    let outcomes = pollster::block_on(plugin.request(vec![
        "android.permission.CAMERA".to_owned(),
        "android.permission.RECORD_AUDIO".to_owned(),
    ]))
    .unwrap();
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0].permission, "android.permission.CAMERA");
    assert_eq!(outcomes[0].status, PermissionStatus::Granted);
    assert_eq!(outcomes[1].permission, "android.permission.RECORD_AUDIO");
    backend.join().unwrap();
}

#[test]
fn should_show_rationale_round_trips_bool() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        let env = outbound.recv().expect("call");
        let Frame::Call {
            call_id, method, ..
        } = env.frame
        else {
            panic!("expected call");
        };
        assert_eq!(method, "should_show_rationale");
        let bytes = codec::encode(&true).unwrap();
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Ok(bytes),
            }))
            .unwrap();
    });

    let plugin = PermissionsClient::from_runtime(&rt).expect("declared");
    let show =
        pollster::block_on(plugin.should_show_rationale("android.permission.CAMERA".to_owned()))
            .unwrap();
    assert!(show);
    backend.join().unwrap();
}
