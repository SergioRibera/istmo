//! The ScreenCaptureKit backend against a real Mac. It needs the Screen
//! Recording grant for the process running the tests (the terminal, or
//! the IDE) and a relaunch after granting it, so it runs only with
//! `ISTMO_SC_TEST_MACOS` set; `ISTMO_SC_TEST_MACOS_AUDIO` adds the
//! audio-only test (something should be playing). The picker needs a
//! main run loop, which libtest has not: it is `tests/macos_picker.rs`.

#![cfg(all(target_os = "macos", feature = "macos-sck"))]

mod common;

use std::time::{Duration, Instant};

use istmo_core::{Overflow, SinkPolicy};
use istmo_screen_capture::{
    AccessRequest, AccessScope, AccessStatus, AudioConfig, AudioSource, BackendId, BufferMemory,
    CaptureError, CaptureEvent, ConsentConfig, CopyStage, Exhaustion, FormatChoice, FormatRequest,
    FormatTier, FrameRate, IoSurfaceFrame, PickerOptions, PixelFormat, PixelLayout, PtsClock,
    SampleFormat, ScreenCaptureClientExt, ScreenshotRequest, SessionConfig, SourceFilter,
    SourceKind, SourceListing, SourceSelector,
};

const SCK: BackendId = BackendId::MacosSck;
const GATE: &str = "ISTMO_SC_TEST_MACOS";

fn now_us() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_micros(),
    )
    .expect("fits")
}

fn tier(format: PixelFormat, memory: BufferMemory) -> FormatRequest {
    FormatRequest {
        tiers: vec![FormatTier::new(vec![FormatChoice {
            format,
            memory,
            modifiers: Vec::new(),
        }])],
    }
}

#[test]
fn grant_caps_and_sources() {
    if !common::enabled(GATE) {
        return;
    }
    let live = common::live();
    let access = common::block(live.client.access(AccessScope::ScreenRecording)).expect("access");
    eprintln!("screen recording: {access:?}");
    if access.status != AccessStatus::Granted {
        let asked = common::block(live.client.request_access(AccessRequest {
            scope: AccessScope::ScreenRecording,
        }))
        .expect("request access");
        panic!(
            "grant Screen Recording to this terminal and run again (now {:?})",
            asked.status
        );
    }
    let caps = common::caps(&live, SCK);
    assert_eq!(caps.listing, SourceListing::Enumerable);
    assert!(caps.video.iter().all(|f| f.copies.is_empty()));
    let list = common::block(live.client.sources(SourceFilter::default())).expect("sources");
    for source in &list.sources {
        eprintln!(
            "{:?} {} {:?} {:?}",
            source.kind, source.id.value, source.geometry, source.physical_size
        );
    }
    assert!(
        list.sources
            .iter()
            .any(|s| s.kind == SourceKind::Monitor && s.flags.primary),
        "the main display is listed"
    );
}

#[test]
fn screenshot_pixels_of_every_display() {
    if !common::enabled(GATE) {
        return;
    }
    let live = common::live();
    for selector in [SourceSelector::PrimaryMonitor, SourceSelector::AllMonitors] {
        let request = ScreenshotRequest::of(selector.clone()).backend(common::fixed(SCK));
        let image = common::block(live.client.screenshot_cpu(request, PixelLayout::default()))
            .expect("screenshot");
        eprintln!(
            "{selector:?}: {}x{} stride {}",
            image.width, image.height, image.stride
        );
        assert!(image.width > 0 && image.height > 0);
        assert!(image.bytes.iter().any(|b| *b != 0), "not a blank frame");
    }
}

#[test]
fn iosurface_frames_without_a_copy_then_cpu_frames() {
    if !common::enabled(GATE) {
        return;
    }
    let live = common::live();
    for (format, memory) in [
        (PixelFormat::Bgra8, BufferMemory::IoSurface),
        (PixelFormat::Nv12, BufferMemory::CvPixelBuffer),
        (PixelFormat::Bgra8, BufferMemory::Cpu),
    ] {
        let config = SessionConfig::primary_monitor()
            .backend(common::fixed(SCK))
            .fps(FrameRate::Max(30))
            .formats(tier(format, memory))
            .pool_depth(4);
        let session = common::block(live.client.open_session(
            &live.runtime,
            config,
            SinkPolicy::queue(2),
        ))
        .expect("open");
        let negotiated = session.negotiated().clone();
        eprintln!("{format:?}/{memory:?}: {negotiated:#?}");
        assert_eq!(negotiated.copies, Vec::<CopyStage>::new());
        let mut surfaces = std::collections::HashSet::new();
        let mut frames = 0;
        let deadline = Instant::now() + Duration::from_secs(10);
        while frames < 10 && Instant::now() < deadline {
            let Some(frame) = session.video().try_recv() else {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            };
            assert_eq!(frame.meta.format, format);
            assert_eq!(frame.meta.memory, memory);
            assert_eq!(frame.meta.pts_clock, PtsClock::HostTime);
            assert!(
                now_us().abs_diff(frame.meta.wall_us) < 1_000_000,
                "wall clock"
            );
            if memory == BufferMemory::Cpu {
                let bytes = frame.buffer.as_bytes().expect("CPU frames are mapped");
                let plane = frame.meta.planes[0];
                assert!(bytes.len() as u64 >= u64::from(plane.offset) + plane.size);
            } else {
                let surface = frame
                    .buffer
                    .downcast_ref::<IoSurfaceFrame>()
                    .expect("an IOSurface frame");
                assert!(frame.buffer.platform_ptr().is_some());
                surfaces.insert(surface.surface_id());
                eprintln!(
                    "surface {} slot {:?}",
                    surface.surface_id(),
                    frame.meta.buffer
                );
            }
            frames += 1;
        }
        assert_eq!(frames, 10, "ten frames");
        // SCK's pool size is undocumented: the slots grow with it.
        eprintln!(
            "{} distinct surfaces, queue depth {}",
            surfaces.len(),
            negotiated.pool_depth
        );
        common::block(session.close()).expect("close");
    }
}

#[test]
fn a_consumer_holding_every_frame_exhausts_once_then_reads_back() {
    if !common::enabled(GATE) {
        return;
    }
    let live = common::live();
    for policy in [Exhaustion::Stall, Exhaustion::CopyToCpu] {
        let config = SessionConfig::primary_monitor()
            .backend(common::fixed(SCK))
            .fps(FrameRate::Native)
            .pool_depth(3)
            .exhaustion(policy);
        let session = common::block(live.client.open_session(
            &live.runtime,
            config,
            SinkPolicy::queue(32),
        ))
        .expect("open");
        let mut held = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(frame) = session.video().try_recv() {
                held.push(frame);
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let pooled = held.iter().filter(|f| f.meta.copied.is_none()).count();
        let read_back = held
            .iter()
            .filter(|f| f.meta.copied == Some(CopyStage::ExhaustionReadback))
            .count();
        eprintln!("{policy:?}: {pooled} pooled, {read_back} read back");
        let depth = usize::from(session.negotiated().pool_depth);
        assert!(pooled < depth, "never the last free surface");
        if policy == Exhaustion::CopyToCpu {
            assert!(read_back > 0, "frames keep flowing as read-backs");
        }
        let exhausted = common::events_of(&session)
            .iter()
            .filter(|e| matches!(e, CaptureEvent::Exhausted(_)))
            .count();
        assert_eq!(exhausted, 1, "one Exhausted per episode");
        drop(held);
        common::block(session.close()).expect("close");
    }
}

#[test]
fn the_picker_refuses_at_once_without_a_main_run_loop() {
    if !common::enabled(GATE) {
        return;
    }
    // libtest's main thread runs no run loop: the picker can never show.
    let live = common::live();
    let request = ScreenshotRequest::of(SourceSelector::Picker(PickerOptions::default()))
        .backend(common::fixed(SCK))
        .consent(ConsentConfig {
            allow_picker: true,
            ..ConsentConfig::default()
        });
    let started = Instant::now();
    let refused = common::block(live.client.screenshot_cpu(request, PixelLayout::default()));
    assert!(
        matches!(refused, Err(CaptureError::BackendUnavailable(_))),
        "{refused:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(10), "fails fast");
}

#[test]
fn audio_only_follows_the_wall_clock() {
    if !common::enabled("ISTMO_SC_TEST_MACOS_AUDIO") {
        return;
    }
    let live = common::live();
    let config = AudioConfig::of(AudioSource::System)
        .rate(48_000)
        .channels(2)
        .format(SampleFormat::S16);
    let session = common::block(live.client.open_audio_session(
        &live.runtime,
        config,
        SinkPolicy::queue(512).with_overflow(Overflow::DropNewest),
    ))
    .expect("open audio");
    let started = Instant::now();
    let mut chunks = Vec::new();
    while started.elapsed() < Duration::from_secs(3) {
        match session.audio().try_recv() {
            Some(chunk) => chunks.push(chunk),
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    assert!(!chunks.is_empty(), "audio arrived (is something playing?)");
    for chunk in &chunks {
        assert_eq!(chunk.meta.format, SampleFormat::S16);
        assert!(
            now_us().abs_diff(chunk.meta.wall_us) < 2_000_000,
            "wall clock"
        );
        let bytes = chunk.buffer.as_bytes().expect("PCM");
        assert_eq!(
            bytes.len(),
            chunk.meta.frames as usize * usize::from(chunk.meta.channels) * 2
        );
    }
    eprintln!(
        "{} chunks at {} Hz, {} channels",
        chunks.len(),
        chunks[0].meta.rate,
        chunks[0].meta.channels
    );
}
