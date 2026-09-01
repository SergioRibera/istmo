//! Demo of the `lifecycle` core plugin, driven by the early-event helpers
//! that the native backend would normally call.
//!
//! Run with `cargo run --example lifecycle`.

use std::thread;
use std::time::Duration;

use istmo::plugins::{AppLifecycle, LIFECYCLE_CHANNEL, LifecycleState};
use istmo::{Runtime, codec};

fn main() {
    let init = Runtime::mock();
    let rt = init.runtime;

    // Native side publishes the current state before any subscriber attaches.
    publish(&rt, LifecycleState::Created);
    publish(&rt, LifecycleState::Started);
    publish(&rt, LifecycleState::Resumed);

    let plugin = AppLifecycle::from_runtime(&rt);
    println!(
        "current on attach: {:?}",
        plugin.current().expect("current state"),
    );

    let stream = plugin.stream();

    // Publish more transitions from a background "platform" thread.
    let publisher = rt;
    let bg = thread::spawn(move || {
        for state in [
            LifecycleState::Paused,
            LifecycleState::Stopped,
            LifecycleState::LowMemory,
            LifecycleState::ConfigurationChanged,
            LifecycleState::Destroyed,
        ] {
            thread::sleep(Duration::from_millis(20));
            publish(&publisher, state);
        }
    });

    for _ in 0..6 {
        let state = stream.recv().expect("recv");
        println!("stream: {state:?}");
        if matches!(state, LifecycleState::Destroyed) {
            break;
        }
    }
    bg.join().expect("publisher");
}

fn publish(rt: &Runtime, state: LifecycleState) {
    let bytes = codec::encode(&state).expect("encode state");
    rt.publish_early_latest(LIFECYCLE_CHANNEL, bytes);
}
