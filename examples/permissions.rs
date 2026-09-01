//! Demo of the `permissions` core plugin against an in-process mock backend.
//!
//! Run with `cargo run --example permissions`.

use std::thread;

use istmo::plugins::{PermissionOutcome, PermissionStatus, Permissions};
use istmo::{Envelope, Frame, Runtime, codec};

fn main() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    thread::spawn(move || {
        while let Ok(envelope) = outbound.recv() {
            handle(&rt, envelope);
        }
    });

    let plugin = Permissions::from_runtime(&init.runtime);

    let camera = pollster::block_on(plugin.check("android.permission.CAMERA".to_owned()))
        .expect("check camera");
    println!("check(CAMERA) -> {camera:?}");

    let outcomes = pollster::block_on(plugin.request(vec![
        "android.permission.CAMERA".to_owned(),
        "android.permission.RECORD_AUDIO".to_owned(),
    ]))
    .expect("request");
    for outcome in outcomes {
        println!(
            "request outcome: {} -> {:?}",
            outcome.permission, outcome.status,
        );
    }

    let show =
        pollster::block_on(plugin.should_show_rationale("android.permission.CAMERA".to_owned()))
            .expect("rationale");
    println!("should_show_rationale(CAMERA) -> {show}");
}

fn handle(rt: &std::sync::Arc<Runtime>, envelope: Envelope) {
    let Frame::Call {
        call_id,
        method,
        payload,
        ..
    } = envelope.frame
    else {
        return;
    };
    let result = match method.as_str() {
        "check" => {
            let ((_permission,), _) =
                codec::decode::<(String,)>(&payload).expect("decode check arg");
            Ok(codec::encode(&PermissionStatus::NotDetermined).expect("encode status"))
        }
        "request" => {
            let ((permissions,), _) =
                codec::decode::<(Vec<String>,)>(&payload).expect("decode request arg");
            let outcomes: Vec<PermissionOutcome> = permissions
                .into_iter()
                .map(|permission| PermissionOutcome {
                    permission,
                    status: PermissionStatus::Granted,
                })
                .collect();
            Ok(codec::encode(&outcomes).expect("encode outcomes"))
        }
        "should_show_rationale" => Ok(codec::encode(&true).expect("encode bool")),
        _ => Err(codec::encode(&format!("unknown method: {method}")).expect("encode err")),
    };
    rt.dispatch_inbound(Envelope::new(Frame::Respond { call_id, result }))
        .expect("dispatch response");
}
