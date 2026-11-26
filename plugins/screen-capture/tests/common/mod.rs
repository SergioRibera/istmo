//! The per-backend checklist, run against a real session (headless
//! compositor, Xvfb, or the live desktop). Each backend's integration
//! test gates itself on an environment variable set by its script, so
//! `cargo test` skips it elsewhere.

#![allow(dead_code, reason = "each test binary uses a different part")]
#![allow(
    unreachable_pub,
    reason = "a test-support module shared by several test binaries"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use istmo_core::{
    CallOrigin, CancelToken, Dispatch, Outcome, Runtime, SinkPolicy, StreamItem, codec,
};
use istmo_screen_capture::{
    BackendCaps, BackendId, BackendSelection, CapabilityQuery, CaptureError, CaptureEvent,
    CopyStage, DesktopCapture, Exhaustion, FrameRate, PixelLayout, Requirement, RestoreToken,
    ScreenCaptureClient, ScreenCaptureClientExt, ScreenCaptureHost, ScreenshotRequest,
    SessionConfig, SourceFilter,
};

pub struct Live {
    pub runtime: Arc<Runtime>,
    pub client: Arc<ScreenCaptureClient>,
}

/// A runtime with the desktop host and a local client.
pub fn live() -> Live {
    let runtime = Runtime::mock().runtime;
    runtime.register_host(ScreenCaptureHost::new(DesktopCapture::default()));
    let client = Arc::new(ScreenCaptureClient::from_runtime(&runtime).expect("client"));
    Live { runtime, client }
}

/// Whether the environment this test needs was set up by its script.
pub fn enabled(var: &str) -> bool {
    let on = std::env::var_os(var).is_some();
    if !on {
        eprintln!("skipped: set {var} (the backend's test script does)");
    }
    on
}

pub fn block<F: std::future::Future>(future: F) -> F::Output {
    pollster::block_on(future)
}

/// Open file descriptors of this process.
pub fn open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd").map_or(0, Iterator::count)
}

pub fn wait_until(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while !done() {
        if Instant::now() > deadline {
            eprintln!("timed out waiting for {what}");
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

/// A pixel of the scene with a known colour (upright, logical = buffer
/// pixels at scale 1).
#[derive(Debug, Clone, Copy)]
pub struct Probe {
    pub x: u32,
    pub y: u32,
    pub rgb: [u8; 3],
}

/// What the scene offers the checklist.
pub struct Scene {
    pub backend: BackendId,
    /// Known pixels.
    pub probes: Vec<Probe>,
    /// Make the screen change (for damage-driven producers).
    pub poke: Box<dyn Fn() + Send + Sync>,
}

pub const fn fixed(backend: BackendId) -> BackendSelection {
    BackendSelection::Fixed(backend)
}

pub fn caps(live: &Live, backend: BackendId) -> BackendCaps {
    let report = block(live.client.capabilities(CapabilityQuery {
        backend: Some(backend),
        ..CapabilityQuery::default()
    }))
    .expect("capabilities");
    let caps = report.backends.into_iter().next().expect("one backend");
    eprintln!("capability report: {caps:#?}");
    caps
}

fn events_of(session: &istmo_screen_capture::SessionHandle) -> Vec<CaptureEvent> {
    let Ok(stream) = session.events() else {
        return Vec::new();
    };
    let mut events = Vec::new();
    while let Some(Ok(StreamItem::Event(event))) = stream.try_recv() {
        events.push(event);
    }
    events
}

/// The checklist from the design, against `scene`.
#[expect(clippy::too_many_lines, reason = "one checklist, step by step")]
pub fn checklist(live: &Live, scene: &Scene) {
    let backend = scene.backend;
    let caps = caps(live, backend);
    assert_eq!(caps.backend, backend);

    // Pixels: the known pattern, upright.
    let image = block(live.client.screenshot_cpu(
        ScreenshotRequest::default().backend(fixed(backend)),
        PixelLayout::default(),
    ))
    .expect("screenshot");
    assert!(image.width > 0 && image.height > 0);
    for probe in &scene.probes {
        let at = (probe.y * image.stride + probe.x * 4) as usize;
        let got = &image.bytes[at..at + 3];
        assert_eq!(got, probe.rgb, "pixel ({}, {})", probe.x, probe.y);
    }

    // open → N frames → close, no leaks.
    let fds_before = open_fds();
    {
        let config = SessionConfig::default()
            .backend(fixed(backend))
            .fps(FrameRate::Native)
            .pool_depth(4);
        let session = block(
            live.client
                .open_session(&live.runtime, config, SinkPolicy::latest()),
        )
        .expect("open");
        eprintln!("negotiated: {:#?}", session.negotiated());
        let mut frames = 0;
        let deadline = Instant::now() + Duration::from_secs(10);
        while frames < 5 && Instant::now() < deadline {
            (scene.poke)();
            if let Some(frame) = session.video().try_recv() {
                assert!(frame.meta.wall_us > 0);
                frames += 1;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(frames, 5, "five frames");
        let events = events_of(&session);
        assert!(
            matches!(events.first(), Some(CaptureEvent::Started(_))),
            "{events:?}"
        );
        block(session.close()).expect("close");
    }
    assert!(
        wait_until("fds back", Duration::from_secs(3), || open_fds()
            <= fds_before),
        "fds {} > {fds_before}",
        open_fds()
    );

    // A consumer that never reads: no growth, one Exhausted.
    {
        let config = SessionConfig::default()
            .backend(fixed(backend))
            .fps(FrameRate::Native)
            .pool_depth(3);
        let session = block(
            live.client
                .open_session(&live.runtime, config, SinkPolicy::queue(16)),
        )
        .expect("open");
        let limit = u64::from(session.negotiated().pool_depth.saturating_sub(1));
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            (scene.poke)();
            std::thread::sleep(Duration::from_millis(20));
        }
        let accepted = session.video().stats().accepted;
        assert!(accepted <= limit, "{accepted} frames held, limit {limit}");
        let exhausted = events_of(&session)
            .iter()
            .filter(|e| matches!(e, CaptureEvent::Exhausted(_)))
            .count();
        assert_eq!(exhausted, 1, "one Exhausted per episode");
        // Stall resumes once a buffer comes back.
        drop(session.video().recv());
        let resumed = wait_until("a fresh frame", Duration::from_secs(5), || {
            (scene.poke)();
            session.video().stats().accepted > accepted
        });
        assert!(resumed, "Stall resumes on release");
    }

    // CopyToCpu keeps frames flowing while the consumer holds every
    // buffer it may: past the hold limit, frames are read back.
    {
        let config = SessionConfig::default()
            .backend(fixed(backend))
            .fps(FrameRate::Native)
            .pool_depth(3)
            .exhaustion(Exhaustion::CopyToCpu);
        let session = block(
            live.client
                .open_session(&live.runtime, config, SinkPolicy::queue(64)),
        )
        .expect("CopyToCpu");
        {
            let mut held = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(10);
            while held.len() < 6 && Instant::now() < deadline {
                (scene.poke)();
                if let Some(frame) = session.video().try_recv() {
                    held.push(frame);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(
                held.len(),
                6,
                "frames keep flowing while every buffer is held"
            );
            assert!(
                held.iter()
                    .any(|f| f.meta.copied == Some(CopyStage::ExhaustionReadback)),
                "read-back frames are marked"
            );
        }
    }

    // Sink dropped mid-stream.
    {
        let config = SessionConfig::default()
            .backend(fixed(backend))
            .fps(FrameRate::Native);
        let session = block(
            live.client
                .open_session(&live.runtime, config, SinkPolicy::latest()),
        )
        .expect("open");
        (scene.poke)();
        drop(session);
    }

    // Remote callers are refused.
    let host = ScreenCaptureHost::new(DesktopCapture::default());
    let payload = codec::encode(&(SourceFilter::default(),)).expect("payload");
    let outcome = block(host.dispatch_from(
        CallOrigin::Remote,
        None,
        "sources",
        &payload,
        CancelToken::new(),
    ))
    .expect("dispatch");
    let Outcome::DomainError(bytes) = outcome else {
        panic!("remote sources answered");
    };
    assert_eq!(
        codec::decode::<CaptureError>(&bytes).expect("error").0,
        CaptureError::OriginRefused
    );

    // Tokens never print.
    let token = RestoreToken {
        value: "secret".into(),
        backend,
        plugin_version: istmo_screen_capture::RESTORE_TOKEN_VERSION,
    };
    assert!(!format!("{token:?}").contains("secret"));
}
