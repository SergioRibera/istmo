#![cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]

use istmo_core::Runtime;
use istmo_pen::{
    PEN_PLUGIN_ID, PenClient, PenConfig, backend::PenDesktopHost, publisher::PenPublisher,
};
use raw_window_handle::{
    HandleError, HasWindowHandle, RawWindowHandle, WindowHandle, XlibWindowHandle,
};

struct MockHandle;

impl HasWindowHandle for MockHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // A fake Xlib handle is enough to exercise the pure Rust
        // bookkeeping path — the Windows subclass installer is
        // `cfg(target_os = "windows")`, so on the test host this is a
        // silent no-op.
        let raw = RawWindowHandle::Xlib(XlibWindowHandle::new(0));
        // SAFETY: the handle is inert; no consumer dereferences the
        // zero-valued `Window` id during this test.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

#[test]
fn create_instance_and_open_streams_via_hosted_dispatch() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    rt.declare_plugin(PEN_PLUGIN_ID);

    let publisher = PenPublisher::install(&rt);
    publisher
        .register_window(1, MockHandle)
        .expect("register_window");

    rt.register_host(PenDesktopHost::new(publisher));

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
    let rt = init.runtime.clone();
    rt.declare_plugin(PEN_PLUGIN_ID);

    let publisher = PenPublisher::install(&rt);
    publisher
        .register_window(7, MockHandle)
        .expect("register_window");

    rt.register_host(PenDesktopHost::new(publisher));

    let client = pollster::block_on(PenClient::from_runtime_with(&rt, PenConfig::new(7)))
        .expect("from_runtime_with");
    let caps = pollster::block_on(client.capabilities()).expect("capabilities");

    // Only assert the invariants that hold on every desktop target.
    // Concrete flags are gated on cfg inside PenBackend so they change
    // per host — no static comparison here.
    assert!(!caps.predicted, "predicted stays off until iOS lands");
}

#[test]
fn set_prediction_enabled_returns_ok_on_desktop() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    rt.declare_plugin(PEN_PLUGIN_ID);

    let publisher = PenPublisher::install(&rt);
    publisher
        .register_window(9, MockHandle)
        .expect("register_window");

    rt.register_host(PenDesktopHost::new(publisher));

    let client = pollster::block_on(PenClient::from_runtime_with(&rt, PenConfig::new(9)))
        .expect("from_runtime_with");
    pollster::block_on(client.set_prediction_enabled(true)).expect("set_prediction_enabled");
}
