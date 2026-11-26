//! The portal backend against the live desktop. It shows the portal's
//! dialog (someone has to pick a monitor), so it runs only with
//! `ISTMO_SC_TEST_PORTAL` set. Audio has its own live test in the
//! library (`ISTMO_SC_TEST_PIPEWIRE`), since an audio session through
//! the public API would also need a video backend.

mod common;

use istmo_screen_capture::{
    BackendId, CaptureEvent, ConsentConfig, CopyStage, FrameRate, PixelLayout,
    ScreenCaptureClientExt, ScreenshotRequest, SessionConfig, SourceSelector,
};

const PORTAL: BackendId = BackendId::LinuxPortal;

#[test]
fn portal_reports_without_a_dialog() {
    if !common::enabled("ISTMO_SC_TEST_PORTAL_CAPS") && !common::enabled("ISTMO_SC_TEST_PORTAL") {
        return;
    }
    let live = common::live();
    let caps = common::caps(&live, PORTAL);
    assert_ne!(caps.source_kinds.len(), 0);
    assert!(
        caps.video
            .iter()
            .all(|f| f.copies == [CopyStage::ShmTransport])
    );
}

#[test]
fn portal_screenshot_after_one_dialog() {
    if !common::enabled("ISTMO_SC_TEST_PORTAL") {
        return;
    }
    let live = common::live();
    let request = ScreenshotRequest {
        consent: ConsentConfig {
            allow_picker: true,
            ..ConsentConfig::default()
        },
        ..ScreenshotRequest::of(SourceSelector::PrimaryMonitor).backend(common::fixed(PORTAL))
    };
    let image = common::block(live.client.screenshot_cpu(request, PixelLayout::default()))
        .expect("screenshot");
    eprintln!(
        "portal screenshot: {}x{} stride {}",
        image.width, image.height, image.stride
    );
    assert!(image.width > 0 && image.height > 0);
    assert!(image.bytes.iter().any(|b| *b != 0), "not a blank frame");
}

#[test]
fn portal_session_after_one_dialog() {
    if !common::enabled("ISTMO_SC_TEST_PORTAL_SESSION") {
        return;
    }
    let live = common::live();
    let mut config = SessionConfig::picker()
        .fps(FrameRate::Native)
        .backend(common::fixed(PORTAL));
    config.consent.allow_picker = true;
    let session = common::block(live.client.open_session(
        &live.runtime,
        config,
        istmo_core::SinkPolicy::latest(),
    ))
    .expect("open");
    eprintln!("negotiated: {:#?}", session.negotiated());
    let frame = session.video().recv().expect("a frame");
    assert!(frame.meta.wall_us > 0);
    let events = session.events().expect("events");
    assert!(matches!(
        events.try_recv(),
        Some(Ok(istmo_core::StreamItem::Event(CaptureEvent::Started(_))))
    ));
    common::block(session.close()).expect("close");
}
