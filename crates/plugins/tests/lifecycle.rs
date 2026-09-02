//! `AppLifecycle` plugin: verifies late-subscriber replay and live updates via
//! the runtime's [`LatestValueSlot`].
//!
//! [`LatestValueSlot`]: istmo_core::early_events::LatestValueSlot

use istmo_core::{Runtime, codec};
use istmo_plugins::{AppLifecycle, LIFECYCLE_CHANNEL, LifecycleState};

fn publish(rt: &std::sync::Arc<Runtime>, state: LifecycleState) {
    let bytes = codec::encode(&state).expect("encode");
    rt.publish_early_latest(LIFECYCLE_CHANNEL, bytes);
}

#[test]
fn late_subscriber_receives_retained_state_first() {
    let init = Runtime::mock();
    let rt = init.runtime;

    publish(&rt, LifecycleState::Started);
    publish(&rt, LifecycleState::Resumed);

    let plugin = AppLifecycle::from_runtime(&rt).expect("declared");
    assert_eq!(plugin.current().unwrap(), Some(LifecycleState::Resumed));

    let stream = plugin.stream();
    assert_eq!(stream.recv().unwrap(), LifecycleState::Resumed);

    publish(&rt, LifecycleState::Paused);
    assert_eq!(stream.recv().unwrap(), LifecycleState::Paused);
}

#[test]
fn subscriber_receives_configuration_and_low_memory_events() {
    let init = Runtime::mock();
    let rt = init.runtime;

    let plugin = AppLifecycle::from_runtime(&rt).expect("declared");
    let stream = plugin.stream();
    publish(&rt, LifecycleState::LowMemory);
    assert_eq!(stream.recv().unwrap(), LifecycleState::LowMemory);
    publish(&rt, LifecycleState::ConfigurationChanged);
    assert_eq!(stream.recv().unwrap(), LifecycleState::ConfigurationChanged);
}

#[test]
fn multiple_subscribers_observe_the_same_updates() {
    let init = Runtime::mock();
    let rt = init.runtime;

    let plugin = AppLifecycle::from_runtime(&rt).expect("declared");
    let stream_a = plugin.stream();
    let stream_b = plugin.stream();
    publish(&rt, LifecycleState::Stopped);
    assert_eq!(stream_a.recv().unwrap(), LifecycleState::Stopped);
    assert_eq!(stream_b.recv().unwrap(), LifecycleState::Stopped);
}

#[test]
fn current_is_none_before_any_publish() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let plugin = AppLifecycle::from_runtime(&rt).expect("declared");
    assert!(plugin.current().unwrap().is_none());
}
