use istmo_core::{Runtime, codec};
use istmo_plugins::{NETWORK_STATE_CHANNEL, LinkKind, NetworkState, NetworkStateClient};

fn sample() -> NetworkState {
    NetworkState {
        online: true,
        link: LinkKind::Wifi,
        metered: false,
        expensive: false,
        constrained: false,
    }
}

fn publish(rt: &std::sync::Arc<Runtime>, state: &NetworkState) {
    let bytes = codec::encode(state).expect("encode");
    rt.publish_early_latest(NETWORK_STATE_CHANNEL, bytes);
}

#[test]
fn current_is_none_before_publish() {
    let init = Runtime::mock();
    let plugin = NetworkStateClient::from_runtime(&init.runtime).expect("declared");
    assert!(plugin.current().unwrap().is_none());
}

#[test]
fn late_subscriber_sees_retained() {
    let init = Runtime::mock();
    let rt = init.runtime;
    publish(&rt, &sample());
    let plugin = NetworkStateClient::from_runtime(&rt).expect("declared");
    assert_eq!(plugin.current().unwrap(), Some(sample()));
    assert_eq!(plugin.updates().recv().unwrap(), sample());
}

#[test]
fn updates_stream_delivers_transitions() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let plugin = NetworkStateClient::from_runtime(&rt).expect("declared");
    let stream = plugin.updates();

    publish(&rt, &sample());
    assert_eq!(stream.recv().unwrap(), sample());

    let offline = NetworkState {
        online: false,
        link: LinkKind::None,
        metered: false,
        expensive: false,
        constrained: false,
    };
    publish(&rt, &offline);
    assert_eq!(stream.recv().unwrap(), offline);

    let cellular = NetworkState {
        online: true,
        link: LinkKind::Cellular,
        metered: true,
        expensive: true,
        constrained: false,
    };
    publish(&rt, &cellular);
    assert_eq!(stream.recv().unwrap(), cellular);
}
