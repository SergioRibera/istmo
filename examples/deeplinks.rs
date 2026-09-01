//! Demo of the `deeplinks` core plugin: pre-launch buffer + live delivery.
//!
//! Run with `cargo run --example deeplinks`.

use std::thread;
use std::time::Duration;

use istmo::plugins::{DEEPLINKS_CHANNEL, DeepLink, DeepLinks};
use istmo::{Runtime, codec};

fn main() {
    let init = Runtime::mock();
    let rt = init.runtime;

    // Native side receives two cold-start links before Rust subscribes.
    publish(
        &rt,
        &DeepLink {
            uri: "https://example.com/cold-1".to_owned(),
            source: Some("intent".to_owned()),
            received_at_ms: Some(1),
        },
    );
    publish(
        &rt,
        &DeepLink {
            uri: "istmo://open?tab=home".to_owned(),
            source: Some("universal-link".to_owned()),
            received_at_ms: Some(2),
        },
    );

    let plugin = DeepLinks::from_runtime(&rt);
    let stream = plugin.stream();

    // Drain the pre-main buffer.
    while let Some(result) = stream.try_recv() {
        let link = result.expect("decode link");
        println!("prelaunch: {link:?}");
    }

    // Publish a live link a moment later.
    let bg = thread::spawn(move || {
        thread::sleep(Duration::from_millis(30));
        publish(
            &rt,
            &DeepLink {
                uri: "istmo://open?tab=settings".to_owned(),
                source: None,
                received_at_ms: None,
            },
        );
    });

    let live = stream.recv().expect("live link");
    println!("live: {live:?}");
    bg.join().expect("publisher");
}

fn publish(rt: &Runtime, link: &DeepLink) {
    let bytes = codec::encode(link).expect("encode link");
    rt.publish_early_queue(DEEPLINKS_CHANNEL, 16, bytes);
}
