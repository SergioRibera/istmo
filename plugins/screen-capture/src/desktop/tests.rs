//! Host tests over the fake backend: selection, screenshots (composite
//! included), sessions, pools, events and origin checks.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use istmo_core::{CallOrigin, CancelToken, Dispatch, Outcome, Runtime, SinkPolicy, codec};

use super::DesktopCapture;
use super::engine::Engine;
use super::fake::{self, Counters, Fake};
use crate::policy::CapturePolicy;
use crate::types::{
    Availability, BackendId, BackendSelection, CaptureError, CaptureEvent, CompositeConfig,
    CompositeScale, ConsentConfig, CopyStage, CpuPixelFormat, Exhaustion, FrameRate, PixelLayout,
    Rect, RegionSelector, RegionSpace, Requirement, RestoreToken, ScreenshotRequest, SessionConfig,
    SessionUpdate, SourceFilter, SourceListing, SourceSelector, StopReason,
};
use crate::{ScreenCaptureClient, ScreenCaptureClientExt, ScreenCaptureHost};

const WLR: BackendId = BackendId::LinuxWlrScreencopy;

struct Rig {
    runtime: Arc<Runtime>,
    client: Arc<ScreenCaptureClient>,
    counters: Arc<Counters>,
}

fn two_monitors() -> Vec<crate::types::Source> {
    vec![
        fake::monitor(WLR, 0, Rect::new(0, 0, 8, 4), 1000),
        fake::monitor(WLR, 1, Rect::new(8, 0, 4, 2), 2000),
    ]
}

fn rig_with(fakes: Vec<Fake>, policy: CapturePolicy) -> Rig {
    let counters = fakes
        .first()
        .map_or_else(Default::default, |f| Arc::clone(&f.counters));
    let order: Vec<BackendId> = fakes.iter().map(|f| f.id).collect();
    let backends: Vec<Arc<dyn super::backend::Backend>> = fakes
        .into_iter()
        .map(|f| Arc::new(f) as Arc<dyn super::backend::Backend>)
        .collect();
    let engine = Engine::new(
        backends,
        None,
        vec![BackendId::LinuxExtImageCopy, WLR, BackendId::LinuxPortal],
        order,
        None,
        policy,
    );
    let runtime = Runtime::mock().runtime;
    runtime.register_host(ScreenCaptureHost::new(DesktopCapture::from_engine(engine)));
    let client = Arc::new(ScreenCaptureClient::from_runtime(&runtime).expect("client"));
    Rig {
        runtime,
        client,
        counters,
    }
}

fn rig() -> Rig {
    rig_with(
        vec![Fake::new(WLR, two_monitors())],
        CapturePolicy::default(),
    )
}

fn block<F: std::future::Future>(future: F) -> F::Output {
    pollster::block_on(future)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn backends_list_compiled_and_missing_ones() {
    let rig = rig();
    assert!(rig.client.is_local(), "the typed shortcut is installed");
    let statuses = block(rig.client.backends()).expect("backends");
    assert_eq!(statuses[0].backend, WLR);
    assert_eq!(statuses[0].availability, Availability::Ready);
    assert!(statuses.iter().any(|s| s.backend == BackendId::LinuxPortal
        && matches!(s.availability, Availability::NotCompiled(_))));
}

#[test]
fn nothing_compiled_answers_no_backend_compiled() {
    let rig = rig_with(Vec::new(), CapturePolicy::default());
    let err = block(
        rig.client
            .screenshot_cpu(ScreenshotRequest::default(), PixelLayout::default()),
    )
    .expect_err("no backend");
    assert_eq!(err, CaptureError::NoBackendCompiled);
    let err = CaptureError::from(
        block(
            rig.client
                .capabilities(crate::types::CapabilityQuery::default()),
        )
        .expect_err("caps"),
    );
    assert_eq!(err, CaptureError::NoBackendCompiled);
}

#[test]
fn a_screenshot_is_queued_before_the_call_returns() {
    let rig = rig();
    let sink = rig
        .runtime
        .open_frame_sink(SinkPolicy::latest())
        .expect("sink");
    let info = block(
        rig.client
            .screenshot(ScreenshotRequest::default(), sink.id()),
    )
    .expect("shot");
    assert_eq!(info.frames, 1);
    let frame = sink.try_recv().expect("already queued");
    assert_eq!(frame.meta.source_id.value, "out-0");
    assert!(frame.meta.wall_us > 0);
    assert_eq!(frame.buffer.as_bytes().map(<[u8]>::len), Some(8 * 4 * 4));
}

#[test]
fn screenshot_pixels_are_raw_and_upright() {
    let rig = rig();
    let image = block(
        rig.client
            .screenshot_cpu(ScreenshotRequest::default(), PixelLayout::default()),
    )
    .expect("pixels");
    assert_eq!((image.width, image.height, image.stride), (8, 4, 32));
    assert_eq!(image.format, CpuPixelFormat::Rgba8);
    assert_eq!(image.bytes[0], fake::red(0, 0, 0));
    assert_eq!(
        image.bytes[(8 + 3) * 4],
        fake::red(0, 3, 1),
        "row 1, column 3"
    );
}

#[test]
fn rotated_monitors_are_turned_for_pixels() {
    let mut monitor = fake::monitor(WLR, 0, Rect::new(0, 0, 4, 2), 1000);
    monitor.orientation = fake::ROT90;
    let rig = rig_with(
        vec![Fake::new(WLR, vec![monitor])],
        CapturePolicy::default(),
    );
    let image = block(
        rig.client
            .screenshot_cpu(ScreenshotRequest::default(), PixelLayout::default()),
    )
    .expect("pixels");
    assert_eq!((image.width, image.height), (4, 2));
    // Upright (0, 0) is buffer (0, 3) of the 2x4 buffer.
    assert_eq!(image.bytes[0], fake::red(0, 0, 3));
}

#[test]
fn all_monitors_compose_into_one_frame() {
    let rig = rig();
    let request = ScreenshotRequest::of(SourceSelector::AllMonitors);
    let (info, frames) =
        block(rig.client.screenshot_frames(&rig.runtime, request.clone())).expect("composite");
    assert_eq!(info.frames, 1);
    let frame = &frames[0];
    // Highest scale (2.0): 12x4 logical → 24x8.
    assert_eq!((frame.meta.size.width, frame.meta.size.height), (24, 8));
    assert_eq!(frame.meta.copied, Some(CopyStage::Composite));
    let composite = frame.meta.composite.as_ref().expect("parts listed");
    assert_eq!(composite.parts.len(), 2);
    assert_eq!(composite.parts[1].rect, Rect::new(16, 0, 8, 4));
    assert!(info.negotiated.copies.contains(&CopyStage::Composite));

    let image = block(rig.client.screenshot_cpu(request, PixelLayout::default())).expect("pixels");
    let px = |x: u32, y: u32| image.bytes[((y * image.width + x) * 4) as usize..][..4].to_vec();
    // Monitor 0 is upsampled 2x: composite (2, 2) shows its (1, 1).
    assert_eq!(px(2, 2)[0], fake::red(0, 1, 1));
    // Monitor 1 is at its own scale: composite (16 + 3, 1) is its (3, 1).
    assert_eq!(px(19, 1)[0], fake::red(1, 3, 1));
    // Below monitor 1 is a gap: transparent.
    assert_eq!(px(20, 6)[3], 0);
}

#[test]
fn composite_false_gives_a_frame_per_monitor() {
    let rig = rig();
    let request = ScreenshotRequest {
        composite: false,
        ..ScreenshotRequest::of(SourceSelector::AllMonitors)
    };
    let (info, frames) =
        block(rig.client.screenshot_frames(&rig.runtime, request.clone())).expect("frames");
    assert_eq!(info.frames, 2);
    assert_eq!(frames[1].meta.source_id.value, "out-1");
    let err = block(rig.client.screenshot_cpu(request, PixelLayout::default()))
        .expect_err("one image only");
    assert!(
        matches!(err, CaptureError::Unsatisfiable(ref u) if u.requirements == [Requirement::MultiMonitor])
    );
}

#[test]
fn a_region_inside_one_monitor_is_a_crop() {
    let rig = rig();
    let request = ScreenshotRequest::of(SourceSelector::Region(RegionSelector {
        rect: Rect::new(9, 0, 2, 1),
        space: RegionSpace::Global,
    }));
    let (info, frames) =
        block(rig.client.screenshot_frames(&rig.runtime, request.clone())).expect("crop");
    assert_eq!(info.frames, 1);
    // Monitor 1 at 2.0: logical (1, 0, 2, 1) → buffer (2, 0, 4, 2).
    assert_eq!(frames[0].meta.content_rect, Rect::new(2, 0, 4, 2));
    let image = block(rig.client.screenshot_cpu(request, PixelLayout::default())).expect("pixels");
    assert_eq!((image.width, image.height), (4, 2));
    assert_eq!(image.bytes[0], fake::red(1, 2, 0));
}

#[test]
fn a_region_across_monitors_is_a_composite() {
    let rig = rig();
    let request = ScreenshotRequest {
        composite_config: CompositeConfig {
            scale: CompositeScale::Logical,
            ..CompositeConfig::default()
        },
        ..ScreenshotRequest::of(SourceSelector::Region(RegionSelector {
            rect: Rect::new(7, 0, 2, 1),
            space: RegionSpace::Global,
        }))
    };
    let image = block(rig.client.screenshot_cpu(request, PixelLayout::default())).expect("pixels");
    assert_eq!((image.width, image.height), (2, 1));
    assert_eq!(image.bytes[0], fake::red(0, 7, 0));
    // Monitor 1 at 2.0 sampled at logical scale: logical (0, 0) is its
    // buffer pixel (1, 1) (centre sampling of a 2x2 block).
    assert_eq!(image.bytes[4], fake::red(1, 1, 1));
}

#[test]
fn sources_and_unknown_ids() {
    let rig = rig();
    let list = block(rig.client.sources(SourceFilter::default())).expect("sources");
    assert_eq!(list.listing, SourceListing::Enumerable);
    assert_eq!(list.sources.len(), 2);
    let mut id = list.sources[0].id.clone();
    id.value = "gone".into();
    let err = block(rig.client.screenshot_cpu(
        ScreenshotRequest::of(SourceSelector::Id(id.clone())),
        PixelLayout::default(),
    ))
    .expect_err("unknown");
    assert_eq!(err, CaptureError::SourceNotFound(id));
}

#[test]
fn a_picker_only_backend_needs_allow_picker() {
    let mut portal = Fake::new(BackendId::LinuxPortal, two_monitors());
    portal.listing = SourceListing::PickerOnly;
    let rig = rig_with(vec![portal], CapturePolicy::default());
    let err = block(
        rig.client
            .screenshot_cpu(ScreenshotRequest::default(), PixelLayout::default()),
    )
    .expect_err("consent");
    assert_eq!(err, CaptureError::ConsentRequired);
    let request = ScreenshotRequest {
        consent: ConsentConfig {
            allow_picker: true,
            ..ConsentConfig::default()
        },
        ..ScreenshotRequest::default()
    };
    block(rig.client.screenshot_cpu(request, PixelLayout::default())).expect("picker allowed");
}

#[test]
fn open_streams_frames_then_close_returns_every_buffer() {
    let rig = rig();
    let config = SessionConfig::default()
        .fps(FrameRate::Native)
        .pool_depth(4);
    let session = block(
        rig.client
            .open_session(&rig.runtime, config, SinkPolicy::queue(2)),
    )
    .expect("open");
    assert_eq!(session.negotiated().pool_depth, 4);
    let events = session.events().expect("events");
    let mut seqs = Vec::new();
    for _ in 0..5 {
        let frame = session.video().recv().expect("frame");
        seqs.push(frame.meta.seq);
    }
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    block(session.close()).expect("close");
    assert_eq!(
        rig.counters.live_threads.load(Ordering::SeqCst),
        0,
        "capture thread joined"
    );
    let got: Vec<CaptureEvent> = std::iter::from_fn(|| {
        events.recv().ok().and_then(|o| match o {
            istmo_core::StreamItem::Event(e) => Some(e),
            _ => None,
        })
    })
    .collect();
    assert!(
        matches!(got.first(), Some(CaptureEvent::Started(_))),
        "{got:?}"
    );
    assert!(
        matches!(got.last(), Some(CaptureEvent::Stopped(StopReason::Closed))),
        "{got:?}"
    );
}

#[test]
fn dropping_the_handle_closes_the_session() {
    let rig = rig();
    let session = block(rig.client.open_session(
        &rig.runtime,
        SessionConfig::default(),
        SinkPolicy::latest(),
    ))
    .expect("open");
    let _first = session.video().recv().expect("frame");
    drop(session);
    wait_until("the capture thread to stop", || {
        rig.counters.live_threads.load(Ordering::SeqCst) == 0
    });
}

#[test]
fn a_consumer_that_never_reads_exhausts_once_and_stalls() {
    let rig = rig();
    // A sink deeper than the pool: frames pile up in the sink.
    let config = SessionConfig::default()
        .fps(FrameRate::Native)
        .pool_depth(3);
    let session = block(
        rig.client
            .open_session(&rig.runtime, config, SinkPolicy::queue(8)),
    )
    .expect("open");
    let events = session.events().expect("events");
    wait_until("two frames in the sink", || {
        session.video().stats().accepted >= 2
    });
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(
        session.video().stats().accepted,
        2,
        "never the last free buffer"
    );
    let mut exhausted = 0;
    while let Some(Ok(istmo_core::StreamItem::Event(event))) = events.try_recv() {
        if matches!(event, CaptureEvent::Exhausted(_)) {
            exhausted += 1;
        }
    }
    assert_eq!(exhausted, 1, "one event per episode");
    // Reading frees a buffer and frames flow again.
    drop(session.video().recv());
    wait_until("a fresh frame", || session.video().stats().accepted >= 3);
}

#[test]
fn copy_to_cpu_keeps_frames_flowing_while_the_consumer_holds_everything() {
    let rig = rig();
    let config = SessionConfig::default()
        .fps(FrameRate::Native)
        .pool_depth(3)
        .exhaustion(Exhaustion::CopyToCpu);
    let session = block(
        rig.client
            .open_session(&rig.runtime, config, SinkPolicy::queue(64)),
    )
    .expect("open");
    let held: Vec<_> = (0..2)
        .map(|_| session.video().recv().expect("frame"))
        .collect();
    let next = session.video().recv().expect("a read-back frame");
    assert_eq!(next.meta.copied, Some(CopyStage::ExhaustionReadback));
    assert!(rig.counters.readbacks.load(Ordering::SeqCst) >= 1);
    drop(held);
}

#[test]
fn a_dropped_sink_stops_the_capture() {
    let rig = rig();
    let session = block(rig.client.open_session(
        &rig.runtime,
        SessionConfig::default().fps(FrameRate::Native),
        SinkPolicy::latest(),
    ))
    .expect("open");
    let id = session.id();
    let SessionParts { video, .. } = into_parts(session);
    drop(video);
    wait_until("the capture thread to stop", || {
        rig.counters.live_threads.load(Ordering::SeqCst) == 0
    });
    block(rig.client.close(id)).expect("close after the sink went away");
}

struct SessionParts {
    video: istmo_core::FrameSink<crate::types::FrameMeta>,
    _session: istmo_core::NativeHandleId,
}

fn into_parts(session: crate::SessionHandle) -> SessionParts {
    let id = session.id();
    let (video, handle) = session.into_parts_for_test();
    std::mem::forget(handle);
    SessionParts {
        video,
        _session: id,
    }
}

#[test]
fn a_lost_source_ends_the_sink_with_the_reason() {
    let mut fake = Fake::new(WLR, two_monitors());
    fake.frames = 3;
    let rig = rig_with(vec![fake], CapturePolicy::default());
    let session = block(rig.client.open_session(
        &rig.runtime,
        SessionConfig::default().fps(FrameRate::Native),
        SinkPolicy::queue(8),
    ))
    .expect("open");
    let frames = std::iter::from_fn(|| session.video().recv()).count();
    assert_eq!(frames, 3, "queued frames are delivered first");
    let reason = session.video().end_reason().expect("ended");
    assert!(reason.error().is_some(), "{reason:?}");
}

#[test]
fn multi_monitor_streams_are_unsatisfiable_for_now() {
    let rig = rig();
    let config = SessionConfig::monitor(SourceSelector::AllMonitors);
    let err = block(
        rig.client
            .open_session(&rig.runtime, config, SinkPolicy::latest()),
    )
    .expect_err("composite stream");
    let CaptureError::NoBackend(attempts) = &err else {
        panic!("unexpected {err:?}");
    };
    assert!(matches!(
        &attempts[0].failure,
        crate::types::AttemptFailure::Unsatisfiable(r) if r == &[Requirement::CompositeStreaming]
    ));
    let fixed =
        SessionConfig::monitor(SourceSelector::AllMonitors).backend(BackendSelection::Fixed(WLR));
    let err = block(
        rig.client
            .open_session(&rig.runtime, fixed, SinkPolicy::latest()),
    )
    .expect_err("fixed");
    assert!(
        matches!(err, CaptureError::Unsatisfiable(ref u) if u.requirements == [Requirement::CompositeStreaming])
    );
}

#[test]
fn updates_change_rate_and_pause() {
    let rig = rig();
    let mut session = block(rig.client.open_session(
        &rig.runtime,
        SessionConfig::default().fps(FrameRate::Native),
        SinkPolicy::latest(),
    ))
    .expect("open");
    let negotiated = block(session.update(SessionUpdate::Fps(FrameRate::Max(10)))).expect("fps");
    assert_eq!(negotiated.fps, FrameRate::Max(10));
    block(session.update(SessionUpdate::Pause)).expect("pause");
    std::thread::sleep(Duration::from_millis(150));
    let before = session.video().stats().accepted;
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(session.video().stats().accepted, before, "paused");
    block(session.update(SessionUpdate::Resume)).expect("resume");
    wait_until("frames after resume", || {
        session.video().stats().accepted > before
    });
    let err = block(session.update(SessionUpdate::Formats(
        crate::types::FormatRequest::default(),
    )))
    .expect_err("fake cannot");
    assert!(matches!(err, CaptureError::Unsatisfiable(_)));
}

#[test]
fn max_rate_defers_and_merges() {
    let rig = rig();
    let session = block(rig.client.open_session(
        &rig.runtime,
        SessionConfig::default().fps(FrameRate::Max(20)),
        SinkPolicy::queue(64),
    ))
    .expect("open");
    let start = Instant::now();
    let mut frames = Vec::new();
    while start.elapsed() < Duration::from_millis(500) {
        // Read as they come: holding them would exhaust the pool.
        frames.extend(session.video().try_recv().map(|f| f.meta));
        std::thread::sleep(Duration::from_millis(1));
    }
    // ~10 at 20 Hz over 0.5 s, while the producer made ~200.
    assert!((5..=14).contains(&frames.len()), "{}", frames.len());
    assert!(
        frames.iter().skip(1).any(|m| m.skipped_before > 0),
        "merged frames are counted"
    );
}

#[test]
fn exact_rate_repeats_an_idle_source() {
    let mut fake = Fake::new(WLR, two_monitors());
    fake.frames = 1;
    fake.lose_source = false;
    let rig = rig_with(vec![fake], CapturePolicy::default());
    let config = SessionConfig::default()
        .fps(FrameRate::Exact(50))
        .pool_depth(4);
    let session = block(
        rig.client
            .open_session(&rig.runtime, config, SinkPolicy::queue(64)),
    )
    .expect("open");
    std::thread::sleep(Duration::from_millis(300));
    let frames: Vec<_> = std::iter::from_fn(|| session.video().try_recv()).collect();
    assert!(
        frames.len() >= 8,
        "{} frames at 50 Hz in 0.3 s",
        frames.len()
    );
    assert!(!frames[0].meta.repeat);
    assert!(
        frames[1..].iter().all(|f| f.meta.repeat),
        "an idle source repeats"
    );
    assert_eq!(
        frames[1].meta.buffer, frames[0].meta.buffer,
        "same buffer identity"
    );
}

#[test]
fn upright_turns_cpu_frames_and_declares_the_copy() {
    let mut monitor = fake::monitor(WLR, 0, Rect::new(0, 0, 4, 2), 1000);
    monitor.orientation = fake::ROT90;
    let rig = rig_with(
        vec![Fake::new(WLR, vec![monitor])],
        CapturePolicy::default(),
    );
    let mut config = SessionConfig::default().fps(FrameRate::Native);
    config.video.orientation = crate::types::OrientationHandling::Upright;
    let session = block(
        rig.client
            .open_session(&rig.runtime, config, SinkPolicy::latest()),
    )
    .expect("open");
    assert!(
        session
            .negotiated()
            .copies
            .contains(&CopyStage::OrientationUpright)
    );
    let frame = session.video().recv().expect("frame");
    assert!(frame.meta.orientation.is_upright());
    assert_eq!((frame.meta.size.width, frame.meta.size.height), (4, 2));
    assert_eq!(frame.meta.copied, Some(CopyStage::OrientationUpright));
}

fn dispatch(
    host: &ScreenCaptureHost<DesktopCapture>,
    origin: CallOrigin,
    method: &str,
    payload: &[u8],
) -> Outcome {
    block(host.dispatch_from(origin, None, method, payload, CancelToken::new())).expect("dispatch")
}

fn host(policy: CapturePolicy) -> ScreenCaptureHost<DesktopCapture> {
    let engine = Engine::new(
        vec![Arc::new(Fake::new(WLR, two_monitors()))],
        None,
        Vec::new(),
        vec![WLR],
        None,
        policy,
    );
    ScreenCaptureHost::new(DesktopCapture::from_engine(engine))
}

fn domain_error(outcome: Outcome) -> CaptureError {
    match outcome {
        Outcome::DomainError(bytes) => {
            codec::decode::<CaptureError>(&bytes)
                .expect("domain error")
                .0
        }
        other => panic!("expected a domain error, got {other:?}"),
    }
}

#[test]
fn remote_callers_are_refused_unless_the_policy_opens_the_method() {
    let payload = codec::encode(&(SourceFilter::default(),)).expect("payload");
    let closed = host(CapturePolicy::default());
    assert_eq!(
        domain_error(dispatch(&closed, CallOrigin::Remote, "sources", &payload)),
        CaptureError::OriginRefused
    );
    assert!(matches!(
        dispatch(&closed, CallOrigin::Peer, "sources", &payload),
        Outcome::Ok(_)
    ));
    let open = host(CapturePolicy::default().with_remote_method("sources"));
    assert!(matches!(
        dispatch(&open, CallOrigin::Remote, "sources", &payload),
        Outcome::Ok(_)
    ));
    // Reports stay open to everyone.
    let empty = codec::encode(&()).expect("payload");
    assert!(matches!(
        dispatch(&closed, CallOrigin::Remote, "backends", &empty),
        Outcome::Ok(_)
    ));
}

#[test]
fn the_restore_token_never_prints() {
    let token = RestoreToken {
        value: "secret-portal-token".into(),
        backend: BackendId::LinuxPortal,
        plugin_version: crate::types::RESTORE_TOKEN_VERSION,
    };
    let printed = format!("{token:?}");
    assert!(!printed.contains("secret"), "{printed}");
    let config = ConsentConfig {
        restore_token: Some(token),
        ..ConsentConfig::default()
    };
    assert!(!format!("{config:?}").contains("secret"));
}

#[test]
fn policy_denied_backends_are_reported() {
    let rig = rig_with(
        vec![Fake::new(WLR, two_monitors())],
        CapturePolicy::default().with_allowed_backends(vec![BackendId::LinuxX11]),
    );
    let statuses = block(rig.client.backends()).expect("backends");
    assert!(matches!(
        statuses[0].availability,
        Availability::PolicyDenied(_)
    ));
    let err = block(
        rig.client
            .screenshot_cpu(ScreenshotRequest::default(), PixelLayout::default()),
    )
    .expect_err("denied");
    assert!(matches!(err, CaptureError::NoBackend(_)));
}

#[test]
fn a_backend_outside_auto_is_reachable_only_by_name() {
    let mut fake = Fake::new(WLR, two_monitors());
    fake.joins_auto = false;
    let rig = rig_with(vec![fake], CapturePolicy::default());
    let err = block(
        rig.client
            .screenshot_cpu(ScreenshotRequest::default(), PixelLayout::default()),
    )
    .expect_err("not in Auto");
    assert!(matches!(err, CaptureError::NoBackend(_)), "{err:?}");
    let fixed = ScreenshotRequest::default().backend(BackendSelection::Fixed(WLR));
    block(rig.client.screenshot_cpu(fixed, PixelLayout::default())).expect("by name");
}

fn secret_token() -> RestoreToken {
    RestoreToken {
        value: "secret-portal-token".into(),
        backend: WLR,
        plugin_version: crate::types::RESTORE_TOKEN_VERSION,
    }
}

/// An engine over one fake backend that hands out a restore token.
fn token_engine() -> (Arc<Runtime>, Engine) {
    let mut fake = Fake::new(WLR, two_monitors());
    fake.restore_token = Some(secret_token());
    let engine = Engine::new(
        vec![Arc::new(fake)],
        None,
        Vec::new(),
        vec![WLR],
        None,
        CapturePolicy::default(),
    );
    (Runtime::mock().runtime, engine)
}

fn open_on(
    runtime: &Arc<Runtime>,
    engine: &Engine,
    origin: CallOrigin,
) -> (
    crate::types::CaptureSession,
    istmo_core::FrameSink<crate::types::FrameMeta>,
) {
    let sink = runtime.open_frame_sink(SinkPolicy::queue(2)).expect("sink");
    let sender = runtime.frame_sender(sink.id()).expect("sender");
    let session = engine
        .open(
            runtime,
            &SessionConfig::default().fps(FrameRate::Native),
            &sender,
            None,
            origin,
            &super::offload::Caller::default(),
        )
        .expect("open");
    (session, sink)
}

fn drain_events(
    rx: &flume::Receiver<Result<CaptureEvent, CaptureError>>,
) -> Vec<Result<CaptureEvent, CaptureError>> {
    std::iter::from_fn(|| rx.recv_timeout(Duration::from_millis(50)).ok()).collect()
}

#[test]
fn restore_tokens_never_travel_in_events() {
    let (runtime, engine) = token_engine();
    let (session, _sink) = open_on(&runtime, &engine, CallOrigin::Local);
    assert!(
        session.negotiated.restore_token.is_some(),
        "the opener gets its token once"
    );
    engine
        .update(session.session, &SessionUpdate::Pause, CallOrigin::Local)
        .expect("update");
    let events = drain_events(&engine.events(session.session, CallOrigin::Local));
    assert!(!events.is_empty());
    for event in events {
        match event.expect("event") {
            CaptureEvent::Started(stream) | CaptureEvent::Renegotiated(stream) => {
                assert!(stream.restore_token.is_none(), "{stream:?}");
            }
            _ => {}
        }
    }
}

#[test]
fn sessions_answer_only_the_origin_that_opened_them() {
    let (runtime, engine) = token_engine();
    let (session, _sink) = open_on(&runtime, &engine, CallOrigin::Local);
    let id = session.session;
    for origin in [CallOrigin::Peer, CallOrigin::Remote] {
        assert_eq!(
            engine
                .update(id, &SessionUpdate::Pause, origin)
                .expect_err("foreign update"),
            CaptureError::OriginRefused
        );
        assert_eq!(
            engine.close(id, origin).expect_err("foreign close"),
            CaptureError::OriginRefused
        );
        let refused = drain_events(&engine.events(id, origin));
        assert_eq!(refused, vec![Err(CaptureError::OriginRefused)]);
    }
    engine
        .close(id, CallOrigin::Local)
        .expect("the opener closes");
}

#[test]
fn events_stay_local_whatever_the_policy() {
    let policy = CapturePolicy::default().with_remote_method("events");
    assert!(!policy.admits_remote("events"));
}

#[test]
fn session_ids_are_not_sequential() {
    let (runtime, engine) = token_engine();
    let (a, _sa) = open_on(&runtime, &engine, CallOrigin::Local);
    let (b, _sb) = open_on(&runtime, &engine, CallOrigin::Local);
    assert!(a.session.is_rust_allocated() && b.session.is_rust_allocated());
    assert_ne!(a.session.0.abs_diff(b.session.0), 1, "{a:?} {b:?}");
    let next = runtime.alloc_native_handle_id();
    assert!(
        a.session.0 > next.0 && b.session.0 > next.0,
        "away from the runtime counter"
    );
}

#[test]
fn remote_streams_carry_no_titles_or_tokens() {
    let (runtime, engine) = token_engine();
    let (session, _sink) = open_on(&runtime, &engine, CallOrigin::Remote);
    let stream = session.negotiated;
    assert!(stream.restore_token.is_none());
    assert!(stream.source.name.is_empty());
    assert!(stream.source.app_id.is_none() && stream.source.pid.is_none());
    let (local, _sink) = open_on(&runtime, &engine, CallOrigin::Local);
    assert_eq!(local.negotiated.source.name, "OUT-0");
}

#[test]
fn every_subscriber_learns_the_negotiated_stream() {
    let (runtime, engine) = token_engine();
    let (session, _sink) = open_on(&runtime, &engine, CallOrigin::Local);
    let first = drain_events(&engine.events(session.session, CallOrigin::Local));
    assert!(matches!(first.first(), Some(Ok(CaptureEvent::Started(_)))));
    // Push the `Started` out of the replay window.
    for _ in 0..100 {
        engine
            .update(session.session, &SessionUpdate::Pause, CallOrigin::Local)
            .expect("update");
    }
    let second = drain_events(&engine.events(session.session, CallOrigin::Local));
    assert!(
        matches!(second.first(), Some(Ok(CaptureEvent::Started(stream))) if stream.restore_token.is_none()),
        "{:?}",
        second.first()
    );
}

#[test]
fn oversized_composites_are_unsatisfiable_not_fatal() {
    let far = vec![
        fake::monitor(WLR, 0, Rect::new(0, 0, 8, 4), 1000),
        fake::monitor(WLR, 1, Rect::new(60_000, 60_000, 8, 4), 1000),
    ];
    let far_rig = rig_with(vec![Fake::new(WLR, far)], CapturePolicy::default());
    let err = block(far_rig.client.screenshot_cpu(
        ScreenshotRequest::of(SourceSelector::AllMonitors),
        PixelLayout::default(),
    ))
    .expect_err("far apart");
    assert_eq!(
        err,
        CaptureError::unsatisfiable(Some(WLR), Requirement::CompositeSize)
    );
    // A huge requested scale is clamped, never allocated.
    let rig = rig();
    let request = ScreenshotRequest {
        composite_config: CompositeConfig {
            scale: CompositeScale::Permille(1_000_000),
            ..CompositeConfig::default()
        },
        ..ScreenshotRequest::of(SourceSelector::AllMonitors)
    };
    let image = block(rig.client.screenshot_cpu(request, PixelLayout::default())).expect("clamped");
    assert_eq!(
        (image.width, image.height),
        (96, 32),
        "4x the highest scale"
    );
}

#[test]
fn an_abandoned_open_leaves_no_session_running() {
    let fake = Fake::new(WLR, two_monitors());
    let counters = Arc::clone(&fake.counters);
    let engine = Engine::new(
        vec![Arc::new(fake)],
        None,
        Vec::new(),
        vec![WLR],
        None,
        CapturePolicy::default(),
    );
    let runtime = Runtime::mock().runtime;
    let sink = runtime
        .open_frame_sink::<crate::types::FrameMeta>(SinkPolicy::queue(2))
        .expect("sink");
    let sender = runtime.frame_sender(sink.id()).expect("sender");
    let err = engine
        .open(
            &runtime,
            &SessionConfig::default(),
            &sender,
            None,
            CallOrigin::Local,
            &super::offload::Caller::abandoned(),
        )
        .expect_err("nobody waits for it");
    assert_eq!(err, CaptureError::SessionNotFound);
    assert_eq!(counters.live_threads.load(Ordering::SeqCst), 0);
    assert!(sink.is_closed(), "sink ended");
}

#[test]
fn dropping_a_handle_never_waits_for_the_teardown() {
    let mut fake = Fake::new(WLR, two_monitors());
    fake.teardown = Duration::from_millis(600);
    let rig = rig_with(vec![fake], CapturePolicy::default());
    let session = block(rig.client.open_session(
        &rig.runtime,
        SessionConfig::default(),
        SinkPolicy::latest(),
    ))
    .expect("open");
    let _first = session.video().recv().expect("frame");
    let started = Instant::now();
    drop(session);
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "drop blocked for {:?}",
        started.elapsed()
    );
    wait_until("the teardown on a plugin thread", || {
        rig.counters.live_threads.load(Ordering::SeqCst) == 0
    });
}
