#![cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]

use istmo_core::Runtime;
use istmo_pen::{
    PEN_PLUGIN_ID, PenClient, PenConfig, PenEvent, PenHost, PenHoverEvent, PenSample, PenToolKind,
    backend::PenPublisherFactory, publisher::PenPublisher,
};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, UiKitDisplayHandle, UiKitWindowHandle, WindowHandle,
};

/// A `UIView` handle: every desktop backend ignores it, so registering it
/// exercises the pure Rust bookkeeping without touching a display server.
struct MockHandle;

impl HasWindowHandle for MockHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let raw = RawWindowHandle::UiKit(UiKitWindowHandle::new(std::ptr::NonNull::dangling()));
        // SAFETY: the handle is inert; no desktop backend dereferences a
        // `UIView` pointer.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

impl HasDisplayHandle for MockHandle {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        let raw = RawDisplayHandle::UiKit(UiKitDisplayHandle::new());
        // SAFETY: as above, nothing reads it.
        Ok(unsafe { DisplayHandle::borrow_raw(raw) })
    }
}

#[test]
fn create_instance_and_open_streams_via_hosted_dispatch() {
    let init = Runtime::mock();
    let rt = init.runtime;
    rt.declare_plugin(PEN_PLUGIN_ID);

    let publisher = PenPublisher::install(&rt);
    publisher
        .register_window(1, MockHandle)
        .expect("register_window");

    rt.register_host(PenHost::new(PenPublisherFactory::new(publisher)));

    let client = pollster::block_on(PenClient::from_runtime_with(&rt, PenConfig::new(1)))
        .expect("from_runtime_with");
    assert!(client.instance_id().is_some(), "instance id should be set");

    let events = client.events().expect("events stream");
    let hover = client.hover().expect("hover stream");

    // Streams alive, no publisher activity yet on Linux — poll returns
    // no items, but the channels are open.
    assert!(events.try_recv().is_none());
    assert!(hover.try_recv().is_none());
}

#[test]
fn capabilities_round_trip_through_hosted_dispatch() {
    let init = Runtime::mock();
    let rt = init.runtime;
    rt.declare_plugin(PEN_PLUGIN_ID);

    let publisher = PenPublisher::install(&rt);
    publisher
        .register_window(7, MockHandle)
        .expect("register_window");

    rt.register_host(PenHost::new(PenPublisherFactory::new(publisher)));

    let client = pollster::block_on(PenClient::from_runtime_with(&rt, PenConfig::new(7)))
        .expect("from_runtime_with");
    let caps = pollster::block_on(client.capabilities()).expect("capabilities");

    // Only assert the invariants that hold on every desktop target.
    // Concrete flags are gated on cfg inside PenBackend so they change
    // per host — no static comparison here.
    assert!(!caps.predicted, "predicted stays off until iOS lands");
}

fn zero_sample() -> PenSample {
    PenSample {
        x: 10.0,
        y: 20.0,
        pressure: 0.5,
        tilt_x: 0.0,
        tilt_y: 0.0,
        azimuth: 0.0,
        altitude: 0.0,
        twist: 0.0,
        tangential_pressure: 0.0,
        z_offset: 0.0,
        timestamp_us: 0,
        sequence: 0,
        tool_id: 1,
        tool_kind: PenToolKind::Tip,
        buttons: 0,
    }
}

#[test]
fn push_event_delivers_to_registered_window() {
    let init = Runtime::mock();
    let rt = init.runtime;
    rt.declare_plugin(PEN_PLUGIN_ID);

    let publisher = PenPublisher::install(&rt);
    publisher
        .register_window(11, MockHandle)
        .expect("register_window");
    rt.register_host(PenHost::new(PenPublisherFactory::new(publisher.clone())));

    let client = pollster::block_on(PenClient::from_runtime_with(&rt, PenConfig::new(11)))
        .expect("from_runtime_with");
    let stream = client.events().expect("events stream");

    assert!(!publisher.push_event(999, PenEvent::Down(zero_sample())));

    // The host subscribes when it dispatches the stream call, which may
    // land after `events()` returns; samples before that reach no one.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        assert!(publisher.push_event(11, PenEvent::Down(zero_sample())));
        if let Some(item) = stream.try_recv() {
            let msg = item.expect("stream item");
            assert!(matches!(
                msg,
                istmo_core::StreamItem::Event(PenEvent::Down(_))
            ));
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "pushed event never surfaced",
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn push_hover_delivers_to_registered_window() {
    let init = Runtime::mock();
    let rt = init.runtime;
    rt.declare_plugin(PEN_PLUGIN_ID);

    let publisher = PenPublisher::install(&rt);
    publisher
        .register_window(12, MockHandle)
        .expect("register_window");
    rt.register_host(PenHost::new(PenPublisherFactory::new(publisher.clone())));

    let client = pollster::block_on(PenClient::from_runtime_with(&rt, PenConfig::new(12)))
        .expect("from_runtime_with");
    let stream = client.hover().expect("hover stream");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        assert!(publisher.push_hover(12, PenHoverEvent::ProximityLeave));
        if let Some(item) = stream.try_recv() {
            let msg = item.expect("stream item");
            assert!(matches!(
                msg,
                istmo_core::StreamItem::Event(PenHoverEvent::ProximityLeave)
            ));
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "pushed hover event never surfaced",
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn set_prediction_enabled_returns_ok_on_desktop() {
    let init = Runtime::mock();
    let rt = init.runtime;
    rt.declare_plugin(PEN_PLUGIN_ID);

    let publisher = PenPublisher::install(&rt);
    publisher
        .register_window(9, MockHandle)
        .expect("register_window");

    rt.register_host(PenHost::new(PenPublisherFactory::new(publisher)));

    let client = pollster::block_on(PenClient::from_runtime_with(&rt, PenConfig::new(9)))
        .expect("from_runtime_with");
    pollster::block_on(client.set_prediction_enabled(true)).expect("set_prediction_enabled");
}

#[test]
fn follows_shared_window_registry() {
    let init = Runtime::mock();
    let publisher = PenPublisher::install(&init.runtime);
    let registry = istmo_window::WindowRegistry::default();
    registry
        .register(istmo_window::WindowId(3), &MockHandle)
        .expect("register before follow");
    publisher.follow_registry(&registry).expect("follow");
    assert_eq!(publisher.window_ids(), [3]);

    registry
        .register(istmo_window::WindowId(4), &MockHandle)
        .expect("register after follow");
    let mut ids = publisher.window_ids();
    ids.sort_unstable();
    assert_eq!(ids, [3, 4]);

    registry.unregister(istmo_window::WindowId(3));
    assert_eq!(publisher.window_ids(), [4]);
}

#[test]
fn every_subscriber_sees_every_event() {
    let init = Runtime::mock();
    let publisher = PenPublisher::install(&init.runtime);
    publisher.register_window_id(5);
    let first = publisher.subscribe_events(5).expect("first");
    let second = publisher.subscribe_events(5).expect("second");

    assert!(publisher.push_event(5, PenEvent::Down(zero_sample())));
    assert!(matches!(first.try_recv(), Ok(PenEvent::Down(_))));
    assert!(matches!(second.try_recv(), Ok(PenEvent::Down(_))));
}

#[test]
fn unregister_ends_streams() {
    let init = Runtime::mock();
    let publisher = PenPublisher::install(&init.runtime);
    publisher.register_window_id(6);
    let events = publisher.subscribe_events(6).expect("events");
    let hover = publisher.subscribe_hover(6).expect("hover");

    publisher.unregister_window(6);
    assert!(events.recv().is_err(), "events stream ends");
    assert!(hover.recv().is_err(), "hover stream ends");
    assert!(publisher.subscribe_events(6).is_none());
}

#[test]
fn slow_subscriber_drops_newest_and_counts() {
    let init = Runtime::mock();
    let publisher = PenPublisher::install(&init.runtime);
    publisher.register_window_id(8);
    let events = publisher.subscribe_events(8).expect("events");

    let extra = 5;
    for _ in 0..istmo_pen::publisher::SUBSCRIBER_CAPACITY + extra {
        publisher.push_event(8, PenEvent::Down(zero_sample()));
    }
    assert_eq!(events.len(), istmo_pen::publisher::SUBSCRIBER_CAPACITY);
    assert_eq!(publisher.dropped(8), Some(extra as u64));
}

#[test]
fn pen_error_try_from_keeps_transport_errors() {
    let bytes = istmo_core::codec::encode(&istmo_pen::PenError::NotAttached).expect("encode");
    assert_eq!(
        istmo_pen::PenError::try_from(istmo_core::IstmoError::PluginError { bytes }).ok(),
        Some(istmo_pen::PenError::NotAttached)
    );
    assert!(matches!(
        istmo_pen::PenError::try_from(istmo_core::IstmoError::ChannelClosed),
        Err(istmo_core::IstmoError::ChannelClosed)
    ));
}
