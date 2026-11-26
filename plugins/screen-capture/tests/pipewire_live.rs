//! The portal backend against the live desktop. It shows the portal's
//! dialog (someone has to pick a monitor), so it runs only with
//! `ISTMO_SC_TEST_PORTAL` set. Audio has its own live test in the
//! library (`ISTMO_SC_TEST_PIPEWIRE`), since an audio session through
//! the public API would also need a video backend.

#![cfg(target_os = "linux")]

mod common;

use istmo_screen_capture::{
    BackendId, BufferMemory, CaptureEvent, ConsentConfig, CopyStage, FrameRate, PixelLayout,
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
    for format in &caps.video {
        match format.memory {
            BufferMemory::DmaBuf => assert!(format.copies.is_empty(), "{format:?}"),
            _ => assert_eq!(format.copies, [CopyStage::ShmTransport], "{format:?}"),
        }
    }
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
    let fds_before = common::open_fds();
    let mut config = SessionConfig::picker()
        .fps(FrameRate::Native)
        .pool_depth(4)
        .backend(common::fixed(PORTAL));
    config.consent.allow_picker = true;
    let session = common::block(live.client.open_session(
        &live.runtime,
        config,
        istmo_core::SinkPolicy::queue(8),
    ))
    .expect("open");
    let negotiated = session.negotiated().clone();
    eprintln!("negotiated: {negotiated:#?}");
    let events = session.events().expect("events");
    assert!(matches!(
        events.try_recv(),
        Some(Ok(istmo_core::StreamItem::Event(CaptureEvent::Started(_))))
    ));
    // Frames flow; DMA-BUF ones leave without a copy and reuse the pool.
    let mut slots = Vec::new();
    for _ in 0..20 {
        let frame = session.video().recv().expect("a frame");
        assert!(frame.meta.wall_us > 0);
        assert_eq!(frame.meta.memory, negotiated.memory);
        assert!(frame.meta.copied.is_none(), "{:?}", frame.meta.copied);
        if frame.meta.memory == BufferMemory::DmaBuf {
            assert!(negotiated.copies.is_empty(), "{:?}", negotiated.copies);
            assert!(frame.meta.modifier.is_some());
            let dmabuf = frame.buffer.dmabuf().expect("DMA-BUF backing");
            for plane in &dmabuf.planes {
                let flags = rustix::io::fcntl_getfd(&plane.fd).expect("flags");
                assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
            }
        } else {
            assert!(
                frame
                    .buffer
                    .as_bytes()
                    .is_some_and(|b| b.iter().any(|v| *v != 0))
            );
        }
        if !slots.contains(&frame.meta.buffer) {
            slots.push(frame.meta.buffer);
        }
    }
    eprintln!("buffers seen: {slots:?}");
    // Hold everything: the consumer never gets the producer's last
    // buffer, and frames resume once the held ones drop.
    let mut held = Vec::new();
    let quiet = std::time::Duration::from_secs(2);
    while let Some(frame) = recv_within(session.video(), quiet) {
        held.push(frame);
        assert!(held.len() < 64, "a consumer holding frames stalls the pool");
    }
    eprintln!("held at the limit: {}", held.len());
    drop(held);
    assert!(
        recv_within(session.video(), std::time::Duration::from_secs(5)).is_some(),
        "frames resume once held buffers return"
    );
    common::block(session.close()).expect("close");
    assert!(
        common::wait_until("fds back", std::time::Duration::from_secs(3), || {
            common::open_fds() <= fds_before + 2
        }),
        "fds {} vs {fds_before}",
        common::open_fds()
    );
}

fn recv_within<M>(
    sink: &istmo_core::FrameSink<M>,
    timeout: std::time::Duration,
) -> Option<istmo_core::NativeFrame<M>> {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Some(frame) = sink.try_recv() {
            return Some(frame);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    None
}
