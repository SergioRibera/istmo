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
    SessionUpdate, SourceCapture, SourceFilter, SourceKind, SourceListing, SourceSelector,
    StopReason,
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
    rig_with_audio(fakes, None, policy)
}

fn rig_with_audio(fakes: Vec<Fake>, audio: Option<fake::FakeAudio>, policy: CapturePolicy) -> Rig {
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
        audio.map(|a| Arc::new(a) as Arc<dyn super::backend::AudioBackend>),
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
fn listed_only_windows_name_their_pickers_and_refuse_ids() {
    let mut window = crate::types::Source::new(
        crate::types::SourceId::new(WLR, "toplevel:abc"),
        SourceKind::Window,
        "a window",
    );
    window.capture = SourceCapture::PickerOnly(Vec::new());
    let wlr = || {
        let mut sources = two_monitors();
        sources.push(window.clone());
        Fake::new(WLR, sources)
    };
    let portal = || {
        let mut portal = Fake::new(BackendId::LinuxPortal, Vec::new());
        portal.listing = SourceListing::PickerOnly;
        portal.kinds = vec![SourceKind::Monitor, SourceKind::Window];
        portal
    };
    let rig = rig_with(vec![wlr(), portal()], CapturePolicy::default());
    let list = block(rig.client.sources(SourceFilter::default())).expect("sources");
    let listed = list
        .sources
        .iter()
        .find(|s| s.id == window.id)
        .expect("the window is listed");
    assert_eq!(
        listed.capture,
        SourceCapture::PickerOnly(vec![BackendId::LinuxPortal])
    );
    assert!(
        list.sources
            .iter()
            .filter(|s| s.kind == SourceKind::Monitor)
            .all(crate::types::Source::capturable_by_id)
    );
    let err = block(rig.client.screenshot_cpu(
        ScreenshotRequest::of(SourceSelector::Id(window.id.clone())),
        PixelLayout::default(),
    ))
    .expect_err("listed only");
    assert_eq!(
        err,
        CaptureError::unsatisfiable(Some(WLR), Requirement::WindowById)
    );
    // A policy without the portal leaves no picker to name.
    let rig = rig_with(
        vec![wlr(), portal()],
        CapturePolicy::default().with_allowed_backends(vec![WLR]),
    );
    let list = block(rig.client.sources(SourceFilter {
        kinds: vec![SourceKind::Window],
        ..SourceFilter::default()
    }))
    .expect("sources");
    assert_eq!(list.sources.len(), 1);
    assert_eq!(
        list.sources[0].capture,
        SourceCapture::PickerOnly(Vec::new())
    );
}

#[test]
fn a_source_watch_lists_then_follows_changes_until_dropped() {
    let (updates, script) = flume::unbounded();
    let mut wlr = Fake::new(WLR, two_monitors());
    wlr.watch = Some(script);
    let rig = rig_with(vec![wlr], CapturePolicy::default());
    let mut watch = block(rig.client.source_watch(SourceFilter::default())).expect("watch");
    assert_eq!(watch.sources(), &two_monitors()[..]);
    // The second monitor goes, a window opens.
    let window = crate::types::Source::new(
        crate::types::SourceId::new(WLR, "toplevel:w"),
        SourceKind::Window,
        "w",
    );
    let next = vec![two_monitors()[0].clone(), window.clone()];
    updates.send(next.clone()).expect("script");
    let removed = watch.recv().expect("a change").expect("no error");
    assert_eq!(
        removed,
        crate::types::SourceChange::Removed(two_monitors()[1].id.clone())
    );
    let added = watch.recv().expect("a change").expect("no error");
    assert_eq!(added, crate::types::SourceChange::Added(window));
    assert_eq!(watch.sources(), &next[..]);
    // Dropping the watch ends its thread: only the fake's own receiver
    // is left.
    drop(watch);
    wait_until("the watch thread to end", || updates.receiver_count() == 1);
}

#[test]
fn a_source_watch_reports_what_ended_it() {
    let (updates, script) = flume::unbounded::<Vec<crate::types::Source>>();
    let mut wlr = Fake::new(WLR, two_monitors());
    wlr.watch = Some(script.clone());
    let scripted = rig_with(vec![wlr], CapturePolicy::default());
    let mut watch = block(scripted.client.source_watch(SourceFilter::default())).expect("watch");
    drop((updates, script));
    assert_eq!(
        watch.recv().expect("the end"),
        Err(CaptureError::SourceLost)
    );
    // A backend that cannot watch says so at once.
    let rig = rig();
    let err = block(rig.client.source_watch(SourceFilter::default())).expect_err("no watch");
    assert_eq!(
        err,
        CaptureError::unsatisfiable(Some(WLR), Requirement::Operation)
    );
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
fn a_listing_backend_with_a_picker_shows_it_only_when_allowed() {
    let mut sck = Fake::new(BackendId::MacosSck, two_monitors());
    sck.picker = true;
    let with_picker = rig_with(vec![sck], CapturePolicy::default());
    let picker = SourceSelector::Picker(crate::types::PickerOptions::default());
    let err = block(with_picker.client.screenshot_cpu(
        ScreenshotRequest::of(picker.clone()),
        PixelLayout::default(),
    ))
    .expect_err("consent");
    assert_eq!(err, CaptureError::ConsentRequired);
    let allowed = ConsentConfig {
        allow_picker: true,
        ..ConsentConfig::default()
    };
    let request = ScreenshotRequest {
        consent: allowed.clone(),
        ..ScreenshotRequest::of(picker.clone())
    };
    block(
        with_picker
            .client
            .screenshot_cpu(request, PixelLayout::default()),
    )
    .expect("picker allowed");
    // A listing backend without a picker still refuses the selector.
    let plain = rig();
    let request = ScreenshotRequest {
        consent: allowed,
        ..ScreenshotRequest::of(picker)
    };
    let err =
        block(plain.client.screenshot_cpu(request, PixelLayout::default())).expect_err("no picker");
    assert!(matches!(err, CaptureError::Unsatisfiable(_)), "{err:?}");
}

#[test]
fn sources_listed_after_a_grant_need_it_first() {
    let mut sck = Fake::new(BackendId::MacosSck, two_monitors());
    sck.listing = SourceListing::AfterGrant;
    let rig = rig_with(vec![sck], CapturePolicy::default());
    let err = block(
        rig.client
            .screenshot_cpu(ScreenshotRequest::default(), PixelLayout::default()),
    )
    .expect_err("no grant");
    assert_eq!(err, CaptureError::ConsentRequired);
}
#[test]
fn standing_grants_come_from_the_backend_that_has_them() {
    use crate::types::{AccessScope, AccessStatus, ConsentModel};
    let rig = rig();
    let state = block(rig.client.access(AccessScope::ScreenRecording)).expect("access");
    assert_eq!(state.status, AccessStatus::NotApplicable);
    let mut sck = Fake::new(BackendId::MacosSck, two_monitors());
    sck.access = Some(AccessStatus::RequiresRestart);
    let rig = rig_with(vec![sck], CapturePolicy::default());
    let state = block(rig.client.access(AccessScope::ScreenRecording)).expect("access");
    assert_eq!(state.status, AccessStatus::RequiresRestart);
    assert_eq!(state.model, ConsentModel::SystemSetting);
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
    let config = SessionConfig::of(SourceSelector::AllMonitors);
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
        SessionConfig::of(SourceSelector::AllMonitors).backend(BackendSelection::Fixed(WLR));
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
    assert_ne!(events.len(), 0);
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
    assert_eq!(stream.source.name, "");
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

#[test]
fn repeated_backend_reports_reuse_one_probe() {
    let rig = rig();
    for _ in 0..50 {
        let statuses = block(rig.client.backends()).expect("backends");
        assert_eq!(statuses[0].availability, Availability::Ready);
    }
    assert_eq!(rig.counters.probes.load(Ordering::SeqCst), 1);
}

#[test]
fn a_picker_never_stands_in_for_a_region_or_the_focused_window() {
    let mut portal = Fake::new(BackendId::LinuxPortal, two_monitors());
    portal.listing = SourceListing::PickerOnly;
    let rig = rig_with(vec![portal], CapturePolicy::default());
    let consent = ConsentConfig {
        allow_picker: true,
        ..ConsentConfig::default()
    };
    let cases = [
        (
            SourceSelector::Region(RegionSelector {
                rect: Rect::new(1, 1, 2, 2),
                space: RegionSpace::Global,
            }),
            Requirement::Region,
        ),
        (SourceSelector::Focused, Requirement::SourceSelector),
        (SourceSelector::UnderCursor, Requirement::SourceSelector),
    ];
    for (source, requirement) in cases {
        let request = ScreenshotRequest {
            consent: consent.clone(),
            ..ScreenshotRequest::of(source.clone())
        };
        let err = block(rig.client.screenshot_cpu(request, PixelLayout::default()))
            .expect_err("no silent substitution");
        assert_eq!(
            err,
            CaptureError::unsatisfiable(Some(BackendId::LinuxPortal), requirement),
            "{source:?}"
        );
    }
}

#[test]
fn a_full_drop_newest_sink_is_reported_as_full() {
    let rig = rig();
    let sink = rig
        .runtime
        .open_frame_sink(SinkPolicy::queue(1).with_overflow(istmo_core::Overflow::DropNewest))
        .expect("sink");
    let request = ScreenshotRequest {
        composite: false,
        ..ScreenshotRequest::of(SourceSelector::AllMonitors)
    };
    let err =
        block(rig.client.screenshot(request, sink.id())).expect_err("two frames, room for one");
    assert_eq!(CaptureError::from(err), CaptureError::SinkFull);
    assert!(sink.try_recv().is_some(), "the first frame stays queued");
}

#[test]
fn an_empty_monitor_list_names_the_missing_monitors() {
    let rig = rig();
    let err = block(rig.client.screenshot_cpu(
        ScreenshotRequest::of(SourceSelector::Monitors(Vec::new())),
        PixelLayout::default(),
    ))
    .expect_err("no monitor");
    assert_eq!(
        err,
        CaptureError::unsatisfiable(
            Some(WLR),
            Requirement::Kind(crate::types::SourceKind::Monitor)
        )
    );
}

#[test]
fn a_region_update_at_the_edge_of_i32_does_not_overflow() {
    let offset = vec![fake::monitor(WLR, 0, Rect::new(100, 100, 8, 4), 1000)];
    let engine = Engine::new(
        vec![Arc::new(Fake::new(WLR, offset))],
        None,
        Vec::new(),
        vec![WLR],
        None,
        CapturePolicy::default(),
    );
    let runtime = Runtime::mock().runtime;
    let (session, _sink) = open_on(&runtime, &engine, CallOrigin::Local);
    let monitor = session.negotiated.source.id.clone();
    let err = engine
        .update(
            session.session,
            &SessionUpdate::Region(RegionSelector {
                rect: Rect::new(i32::MAX, i32::MAX, 10, 10),
                space: RegionSpace::MonitorRelative(monitor),
            }),
            CallOrigin::Local,
        )
        .expect_err("outside the monitor");
    assert_eq!(
        err,
        CaptureError::unsatisfiable(Some(WLR), Requirement::Region)
    );
}

#[test]
fn a_frame_that_cannot_be_turned_ends_an_upright_session() {
    let runtime = Runtime::mock().runtime;
    let sink = runtime
        .open_frame_sink::<crate::types::FrameMeta>(SinkPolicy::queue(2))
        .expect("sink");
    let sender = runtime.frame_sender(sink.id()).expect("sender");
    let out = super::output::FrameOut::new(
        WLR,
        sender,
        Arc::new(super::output::EventHub::default()),
        FrameRate::Native,
    );
    out.set_upright(true);
    let source = two_monitors().remove(0);
    let mut meta = fake::meta(&source, crate::types::Size::new(8, 4));
    meta.orientation = fake::ROT90;
    // Far shorter than 8x4 pixels: unreadable, so it cannot be turned.
    let delivery = out.deliver(meta, istmo_core::NativeBuffer::from_bytes(vec![0; 4]));
    assert_eq!(delivery, super::output::Delivery::Closed);
    assert!(sink.try_recv().is_none(), "never delivered unturned");
    let reason = sink.end_reason().expect("ended");
    assert!(format!("{reason:?}").contains("Upright"), "{reason:?}");
}

#[test]
fn exact_pacing_with_a_shallow_pool_keeps_flowing() {
    let rig = rig();
    let config = SessionConfig::default()
        .fps(FrameRate::Exact(60))
        .pool_depth(2);
    let session = block(
        rig.client
            .open_session(&rig.runtime, config, SinkPolicy::latest()),
    )
    .expect("open");
    assert!(
        session.negotiated().pool_depth >= 4,
        "the pacer's pending and repeated frames on top: {}",
        session.negotiated().pool_depth
    );
    let mut fresh = 0;
    let deadline = Instant::now() + Duration::from_secs(2);
    while fresh < 10 && Instant::now() < deadline {
        if let Some(frame) = session.video().try_recv() {
            if !frame.meta.repeat {
                fresh += 1;
            }
        } else {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    assert_eq!(fresh, 10, "new frames keep coming, not only repeats");
}

#[test]
fn a_paced_producer_landing_a_hair_early_is_not_held_a_period() {
    for fps in [FrameRate::Max(10), FrameRate::Exact(10)] {
        let runtime = Runtime::mock().runtime;
        let sink = runtime
            .open_frame_sink::<crate::types::FrameMeta>(SinkPolicy::queue(8))
            .expect("sink");
        let out = super::output::FrameOut::new(
            WLR,
            runtime.frame_sender(sink.id()).expect("sender"),
            Arc::new(super::output::EventHub::default()),
            fps,
        );
        let source = two_monitors().remove(0);
        let frame = || {
            (
                fake::meta(&source, crate::types::Size::new(8, 4)),
                istmo_core::NativeBuffer::from_bytes(vec![0u8; 8 * 4 * 4]),
            )
        };
        let (meta, buffer) = frame();
        assert_eq!(
            out.deliver(meta, buffer),
            super::output::Delivery::Delivered
        );
        // 97 ms into a 100 ms period: within the slack.
        std::thread::sleep(Duration::from_millis(97));
        let (meta, buffer) = frame();
        assert_eq!(
            out.deliver(meta, buffer),
            super::output::Delivery::Delivered,
            "{fps:?}"
        );
        // Right after a delivery: early, deferred to the period's end.
        let (meta, buffer) = frame();
        assert_eq!(out.deliver(meta, buffer), super::output::Delivery::Deferred);
        out.stop(StopReason::Closed);
    }
}

#[test]
fn upright_on_an_upright_source_lists_no_copy() {
    let rig = rig();
    let mut config = SessionConfig::default().fps(FrameRate::Native);
    config.video.orientation = crate::types::OrientationHandling::Upright;
    let session = block(
        rig.client
            .open_session(&rig.runtime, config, SinkPolicy::latest()),
    )
    .expect("open");
    assert!(
        !session
            .negotiated()
            .copies
            .contains(&CopyStage::OrientationUpright),
        "nothing to turn"
    );
    let frame = session.video().recv().expect("frame");
    assert_eq!(frame.meta.copied, None);
}

#[test]
fn a_vanished_source_invalidates_the_probe_cache() {
    let rig = rig();
    block(rig.client.backends()).expect("backends");
    assert_eq!(rig.counters.probes.load(Ordering::SeqCst), 1);
    let gone = crate::types::SourceId::new(WLR, "unplugged");
    let err = block(rig.client.screenshot_cpu(
        ScreenshotRequest::of(SourceSelector::Id(gone.clone())),
        PixelLayout::default(),
    ))
    .expect_err("gone");
    assert_eq!(err, CaptureError::SourceNotFound(gone));
    block(rig.client.backends()).expect("backends");
    assert_eq!(
        rig.counters.probes.load(Ordering::SeqCst),
        2,
        "probed again after the outputs changed"
    );
}

#[test]
fn monitor_sessions_refuse_a_window_id() {
    let mut sources = two_monitors();
    sources[1].kind = crate::types::SourceKind::Window;
    let window = sources[1].id.clone();
    let monitor = sources[0].id.clone();
    let rig = rig_with(vec![Fake::new(WLR, sources)], CapturePolicy::default());
    let err = block(rig.client.open_session(
        &rig.runtime,
        SessionConfig::monitor(window),
        SinkPolicy::latest(),
    ))
    .expect_err("a window is not a monitor");
    assert_eq!(
        err,
        CaptureError::unsatisfiable(
            Some(WLR),
            Requirement::Kind(crate::types::SourceKind::Window)
        )
    );
    let session = block(rig.client.open_session(
        &rig.runtime,
        SessionConfig::monitor(monitor.clone()).cpu_frames(),
        SinkPolicy::latest(),
    ))
    .expect("a monitor");
    assert_eq!(session.negotiated().source.id, monitor);
    assert_eq!(session.negotiated().memory, crate::types::BufferMemory::Cpu);
}

#[test]
fn a_pick_other_than_the_named_source_is_refused() {
    let portal_id = BackendId::LinuxPortal;
    let monitors = vec![
        fake::monitor(portal_id, 0, Rect::new(0, 0, 8, 4), 1000),
        fake::monitor(portal_id, 1, Rect::new(8, 0, 4, 2), 1000),
    ];
    let mut portal = Fake::new(portal_id, monitors);
    portal.listing = SourceListing::PickerOnly;
    // The fake picker always hands back the first monitor.
    let picked = portal.monitors[0].id.clone();
    let other = portal.monitors[1].id.clone();
    let rig = rig_with(vec![portal], CapturePolicy::default());
    let consent = ConsentConfig {
        allow_picker: true,
        ..ConsentConfig::default()
    };
    let shot = |source: SourceSelector| {
        let request = ScreenshotRequest {
            consent: consent.clone(),
            ..ScreenshotRequest::of(source)
        };
        block(rig.client.screenshot_cpu(request, PixelLayout::default()))
    };
    assert_eq!(
        shot(SourceSelector::Id(other.clone())).expect_err("substituted"),
        CaptureError::SourceNotFound(other.clone())
    );
    assert_eq!(
        shot(SourceSelector::Monitors(vec![other.clone()])).expect_err("substituted"),
        CaptureError::SourceNotFound(other.clone())
    );
    assert_eq!(
        shot(SourceSelector::Monitors(vec![
            picked.clone(),
            other.clone()
        ]))
        .expect_err("one short"),
        CaptureError::SourceNotFound(other.clone())
    );
    shot(SourceSelector::Id(picked.clone())).expect("the named source was picked");
    shot(SourceSelector::Monitors(vec![picked])).expect("the named monitor was picked");

    let config = SessionConfig::of(SourceSelector::Id(other.clone())).fps(FrameRate::Native);
    let config = SessionConfig {
        consent: consent.clone(),
        ..config
    };
    let err = block(
        rig.client
            .open_session(&rig.runtime, config, SinkPolicy::queue(2)),
    )
    .map(|_| ())
    .expect_err("substituted");
    assert_eq!(err, CaptureError::SourceNotFound(other));
}

fn audio_rig() -> (Rig, Arc<Counters>) {
    let audio = fake::FakeAudio::default();
    let counters = Arc::clone(&audio.counters);
    let rig = rig_with_audio(
        vec![Fake::new(WLR, two_monitors())],
        Some(audio),
        CapturePolicy::default(),
    );
    (rig, counters)
}

/// The next chunk, waiting up to 5 s.
fn next_chunk(
    session: &crate::AudioSessionHandle,
) -> istmo_core::NativeFrame<crate::types::AudioMeta> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(chunk) = session.audio().try_recv() {
            return chunk;
        }
        assert!(Instant::now() < deadline, "timed out waiting for a chunk");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn an_audio_only_session_has_its_shape_clock_and_events() {
    use crate::types::{AudioConfig, AudioEvent, AudioSource, AudioUpdate, SampleFormat};
    let (rig, counters) = audio_rig();
    let config = AudioConfig::of(AudioSource::System)
        .rate(44_100)
        .channels(2)
        .format(SampleFormat::S16);
    let session = block(
        rig.client
            .open_audio_session(&rig.runtime, config, SinkPolicy::queue(64)),
    )
    .expect("open audio");
    assert_eq!(session.negotiated().rate, 44_100);
    assert_eq!(session.negotiated().format, SampleFormat::S16);
    let first = next_chunk(&session);
    let second = next_chunk(&session);
    assert_eq!(first.meta.rate, 44_100);
    assert!(second.meta.seq > first.meta.seq);
    let now_us = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_micros(),
    )
    .expect("fits");
    assert!(
        now_us.abs_diff(second.meta.wall_us) < 1_000_000,
        "wall_us is UNIX-epoch µs"
    );

    // Mute: chunks keep coming, silent.
    block(session.update(AudioUpdate::Mute)).expect("mute");
    let silent = (0..50).map(|_| next_chunk(&session)).find(|c| {
        c.buffer
            .as_bytes()
            .is_some_and(|b| b.iter().all(|v| *v == 0))
    });
    assert!(silent.is_some(), "muted chunks are zeroed");
    block(session.update(AudioUpdate::Unmute)).expect("unmute");

    // Pause: nothing comes; the first chunk after Resume says so.
    block(session.update(AudioUpdate::Pause)).expect("pause");
    std::thread::sleep(Duration::from_millis(50));
    while session.audio().try_recv().is_some() {}
    std::thread::sleep(Duration::from_millis(50));
    assert!(session.audio().try_recv().is_none(), "paused");
    block(session.update(AudioUpdate::Resume)).expect("resume");
    assert!(next_chunk(&session).meta.discontinuity);

    let events = session.events().expect("events");
    let mut seen = Vec::new();
    while let Some(Ok(istmo_core::StreamItem::Event(event))) = events.try_recv() {
        seen.push(event);
    }
    assert_eq!(
        seen,
        vec![
            AudioEvent::Started(session.negotiated().clone()),
            AudioEvent::Muted,
            AudioEvent::Unmuted,
            AudioEvent::Paused(crate::types::PauseReason::Requested),
            AudioEvent::Resumed,
        ]
    );
    block(session.close()).expect("close");
    wait_until("the audio thread to stop", || {
        counters.live_threads.load(Ordering::SeqCst) == 0
    });
    let Ok(istmo_core::StreamItem::Event(AudioEvent::Stopped(StopReason::Closed))) = events.recv()
    else {
        panic!("the events end with Stopped(Closed)");
    };
}

#[test]
fn dropping_an_audio_handle_stops_it_and_reports_cover_audio() {
    use crate::types::{AudioConfig, AudioSource};
    let (rig, counters) = audio_rig();
    let report = block(
        rig.client
            .capabilities(crate::types::CapabilityQuery::default()),
    )
    .expect("caps");
    let audio = report.audio.expect("an audio report");
    assert_eq!(audio.availability, Availability::Ready);
    assert_ne!(audio.caps.sources, Vec::new());

    let session = block(rig.client.open_audio_session(
        &rig.runtime,
        AudioConfig::of(AudioSource::Node(7)).rate(48_000),
        SinkPolicy::queue(8),
    ))
    .expect("open audio");
    assert_eq!(session.negotiated().rate, 48_000);
    let _chunk = next_chunk(&session);
    drop(session);
    wait_until("the audio thread to stop", || {
        counters.live_threads.load(Ordering::SeqCst) == 0
    });

    let err = block(rig.client.open_audio_session(
        &rig.runtime,
        AudioConfig::of(AudioSource::ExcludeSelf),
        SinkPolicy::queue(8),
    ))
    .expect_err("excluding this process");
    assert_eq!(err, CaptureError::unsatisfiable(None, Requirement::Audio));

    // Without an audio backend.
    let rig = self::rig();
    let err = block(rig.client.open_audio_session(
        &rig.runtime,
        AudioConfig::default(),
        SinkPolicy::queue(8),
    ))
    .expect_err("no audio backend");
    assert_eq!(err, CaptureError::NoBackendCompiled);
    let report = block(
        rig.client
            .capabilities(crate::types::CapabilityQuery::default()),
    )
    .expect("caps");
    assert_eq!(report.audio, None);
}

#[test]
fn audio_sessions_answer_only_their_opener() {
    use crate::types::{AudioConfig, AudioUpdate};
    let (rig, _counters) = audio_rig();
    let session = block(rig.client.open_audio_session(
        &rig.runtime,
        AudioConfig::default(),
        SinkPolicy::queue(8),
    ))
    .expect("open audio");
    // A video-session call on an audio id finds nothing, and the other
    // way round.
    assert_eq!(
        block(rig.client.close(session.id())).map_err(CaptureError::from),
        Err(CaptureError::SessionNotFound)
    );
    assert_eq!(
        block(rig.client.update_audio(
            istmo_core::NativeHandleId(session.id().0 ^ 1),
            AudioUpdate::Pause
        ))
        .map_err(CaptureError::from),
        Err(CaptureError::SessionNotFound)
    );
    // A sink-taking method: the runtime itself refuses remote callers,
    // whatever the policy opens.
    let host = ScreenCaptureHost::new(DesktopCapture::from_engine(Engine::new(
        vec![],
        Some(Arc::new(fake::FakeAudio::default())),
        Vec::new(),
        Vec::new(),
        None,
        CapturePolicy::default().with_remote_method("open_audio"),
    )));
    let payload = codec::encode(&(AudioConfig::default(), 1_u64)).expect("payload");
    let refused = block(host.dispatch_from(
        CallOrigin::Remote,
        None,
        "open_audio",
        &payload,
        CancelToken::new(),
    ));
    assert!(refused.is_err(), "{refused:?}");
}
