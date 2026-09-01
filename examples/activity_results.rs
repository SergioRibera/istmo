//! Demo of the `activity_results` core plugin: launch an intent, receive a
//! result, surface a typed launch error.
//!
//! Run with `cargo run --example activity_results`.

use std::thread;

use istmo::plugins::{
    ActivityLaunchError, ActivityOutcome, ActivityResult, ActivityResults, ExtraValue,
    IntentRequest,
};
use istmo::{Envelope, Frame, Runtime, codec};

fn main() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    thread::spawn(move || {
        let mut launched = 0_u32;
        while let Ok(envelope) = outbound.recv() {
            launched += 1;
            handle(&rt, envelope, launched);
        }
    });

    let plugin = ActivityResults::from_runtime(&init.runtime).expect("declared");

    let view = IntentRequest {
        action: "android.intent.action.VIEW".to_owned(),
        uri: Some("https://example.com".to_owned()),
        component: None,
        mime_type: None,
        categories: vec![],
        extras: vec![("ref".to_owned(), ExtraValue::Text("demo".to_owned()))],
    };
    match pollster::block_on(plugin.launch(view)) {
        Ok(result) => println!("view result: {result:?}"),
        Err(err) => println!("view error: {err:?}"),
    }

    let share = IntentRequest {
        action: "android.intent.action.SEND".to_owned(),
        uri: None,
        component: None,
        mime_type: Some("text/plain".to_owned()),
        categories: vec![],
        extras: vec![("subject".to_owned(), ExtraValue::Text("hi".to_owned()))],
    };
    match pollster::block_on(plugin.launch(share)) {
        Ok(result) => println!("share result: {result:?}"),
        Err(istmo::IstmoError::PluginError { bytes }) => {
            let (decoded, _) =
                codec::decode::<ActivityLaunchError>(&bytes).expect("decode launch err");
            println!("share error: {decoded:?}");
        }
        Err(other) => println!("share transport error: {other}"),
    }
}

fn handle(rt: &std::sync::Arc<Runtime>, envelope: Envelope, launched: u32) {
    let Frame::Call {
        call_id,
        method,
        payload,
        ..
    } = envelope.frame
    else {
        return;
    };
    assert_eq!(method, "launch");
    let ((request,), _) = codec::decode::<(IntentRequest,)>(&payload).expect("decode request");

    let result = match request.action.as_str() {
        "android.intent.action.VIEW" => {
            let ok = ActivityResult {
                outcome: ActivityOutcome::Ok,
                data_uri: Some(format!("content://view/{launched}")),
                extras: vec![],
            };
            Ok(codec::encode(&ok).expect("encode result"))
        }
        _ => Err(codec::encode(&ActivityLaunchError::NoActivityFound).expect("encode err")),
    };
    rt.dispatch_inbound(Envelope::new(Frame::Respond { call_id, result }))
        .expect("dispatch");
}
