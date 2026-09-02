//! `Notifications` plugin: schedule + cancel + authorization round-trips.

use std::thread;

use istmo_core::{Envelope, Frame, Runtime, codec};
use istmo_plugins::{
    NOTIFICATIONS_PLUGIN_ID, NotificationError, NotificationHandle, NotificationImportance,
    NotificationRequest, NotificationsClient,
};

fn sample_request() -> NotificationRequest {
    NotificationRequest {
        title: "Hello".to_owned(),
        body: "Signed in as Skinner".to_owned(),
        channel_id: "auth".to_owned(),
        importance: NotificationImportance::Default,
        delay_seconds: None,
        tag: Some("login".to_owned()),
    }
}

#[test]
fn schedule_round_trips_handle() {
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
            panic!("expected Call")
        };
        assert_eq!(plugin_id, NOTIFICATIONS_PLUGIN_ID);
        assert_eq!(method, "schedule");
        let ((decoded,), _) = codec::decode::<(NotificationRequest,)>(&payload).unwrap();
        assert_eq!(decoded, sample_request());
        let handle = NotificationHandle { id: 42 };
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Ok(codec::encode(&handle).unwrap()),
            }))
            .unwrap();
    });

    let client = NotificationsClient::from_runtime(&rt).expect("declared");
    let handle = pollster::block_on(client.schedule(sample_request())).expect("schedule");
    assert_eq!(handle.id, 42);
    backend.join().unwrap();
}

#[test]
fn permission_denied_surfaces_as_typed_error() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        let env = outbound.recv().expect("call");
        let Frame::Call { call_id, .. } = env.frame else {
            panic!("expected Call")
        };
        let bytes = codec::encode(&NotificationError::PermissionDenied).unwrap();
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Err(bytes),
            }))
            .unwrap();
    });

    let client = NotificationsClient::from_runtime(&rt).expect("declared");
    let err = pollster::block_on(client.schedule(sample_request())).expect_err("permission denied");
    let bytes = match err {
        istmo_core::IstmoError::PluginError { bytes } => bytes,
        other => panic!("expected PluginError, got {other:?}"),
    };
    let (decoded, _) = codec::decode::<NotificationError>(&bytes).unwrap();
    assert_eq!(decoded, NotificationError::PermissionDenied);
    backend.join().unwrap();
}

#[test]
fn is_authorized_returns_bool() {
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
            panic!("expected Call")
        };
        assert_eq!(method, "is_authorized");
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Ok(codec::encode(&true).unwrap()),
            }))
            .unwrap();
    });

    let client = NotificationsClient::from_runtime(&rt).expect("declared");
    let authorized = pollster::block_on(client.is_authorized()).unwrap();
    assert!(authorized);
    backend.join().unwrap();
}
