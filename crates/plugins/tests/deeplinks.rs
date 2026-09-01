//! DeepLinks plugin: verifies pre-main queue drain and live propagation.

use istmo_core::{Runtime, codec};
use istmo_plugins::{DEEPLINKS_CHANNEL, DeepLink, DeepLinks};

fn publish(rt: &std::sync::Arc<Runtime>, link: &DeepLink) {
    let bytes = codec::encode(link).expect("encode");
    rt.publish_early_queue(DEEPLINKS_CHANNEL, 16, bytes);
}

#[test]
fn first_subscriber_drains_prelaunch_buffer_in_order() {
    let init = Runtime::mock();
    let rt = init.runtime;

    let cold = DeepLink {
        uri: "https://example.com/cold".to_owned(),
        source: Some("intent".to_owned()),
        received_at_ms: Some(1),
    };
    let warm = DeepLink {
        uri: "https://example.com/warm".to_owned(),
        source: Some("intent".to_owned()),
        received_at_ms: Some(2),
    };
    publish(&rt, &cold);
    publish(&rt, &warm);

    let plugin = DeepLinks::from_runtime(&rt);
    let stream = plugin.stream();
    assert_eq!(stream.recv().unwrap(), cold);
    assert_eq!(stream.recv().unwrap(), warm);
}

#[test]
fn live_links_after_subscription_reach_the_stream() {
    let init = Runtime::mock();
    let rt = init.runtime;

    let plugin = DeepLinks::from_runtime(&rt);
    let stream = plugin.stream();
    let live = DeepLink {
        uri: "istmo://open?tab=42".to_owned(),
        source: None,
        received_at_ms: None,
    };
    publish(&rt, &live);
    assert_eq!(stream.recv().unwrap(), live);
}

#[test]
fn second_subscriber_gets_no_backlog_only_live() {
    let init = Runtime::mock();
    let rt = init.runtime;

    let first = DeepLink {
        uri: "https://example.com/first".to_owned(),
        source: None,
        received_at_ms: None,
    };
    publish(&rt, &first);

    let plugin = DeepLinks::from_runtime(&rt);
    let stream_a = plugin.stream();
    let stream_b = plugin.stream();

    assert_eq!(stream_a.recv().unwrap(), first);
    assert!(stream_b.try_recv().is_none());

    let live = DeepLink {
        uri: "https://example.com/live".to_owned(),
        source: None,
        received_at_ms: None,
    };
    publish(&rt, &live);
    assert_eq!(stream_a.recv().unwrap(), live);
    assert_eq!(stream_b.recv().unwrap(), live);
}
