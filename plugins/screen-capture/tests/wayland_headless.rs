//! The direct Wayland backends against a real compositor.
//!
//! - `ISTMO_SC_TEST_SWAY=1`: headless sway as `scripts/test-screen-capture-sway.sh`
//!   sets it up (two outputs with known solid backgrounds, the second at
//!   scale 2 and turned 90°), wlr-screencopy and ext-image-copy.
//! - `ISTMO_SC_TEST_NIRI=1`: the live niri session (wlr-screencopy only,
//!   no pixel probes, DMA-BUF zero-copy checks).
//!
//! Both expect `ISTMO_SCREEN_CAPTURE_FULL_COPY=1` so an idle screen still
//! produces frames.

mod common;

use istmo_core::{MemoryKind, SinkPolicy};
use istmo_screen_capture::{
    BackendId, BufferMemory, CaptureEvent, CopyStage, FormatChoice, FormatRequest, FormatTier,
    FrameRate, PixelFormat, PixelLayout, ScreenCaptureClientExt, ScreenshotRequest, SessionConfig,
    SourceFilter, SourceSelector, SyncKind,
};

use common::{Probe, Scene, block, checklist, fixed, live};

const SWAY: &str = "ISTMO_SC_TEST_SWAY";
const NIRI: &str = "ISTMO_SC_TEST_NIRI";

/// The first headless output's background (`#336699`).
const FIRST: [u8; 3] = [0x33, 0x66, 0x99];
/// The second one's (`#cc3300`).
const SECOND: [u8; 3] = [0xcc, 0x33, 0x00];

fn sway_scene(backend: BackendId) -> Scene {
    Scene {
        backend,
        probes: vec![
            Probe {
                x: 10,
                y: 10,
                rgb: FIRST,
            },
            Probe {
                x: 600,
                y: 400,
                rgb: FIRST,
            },
        ],
        poke: Box::new(Damage::default().into_poke()),
    }
}

/// Damages HEADLESS-1 through sway IPC by toggling its background
/// between two nearly equal colours, at most every 100 ms (each change
/// restarts swaybg). ext-image-copy waits for damage and has no
/// full-copy request, so its stream needs this on an idle output.
#[derive(Default)]
struct Damage {
    state: std::sync::Mutex<(bool, Option<std::time::Instant>)>,
}

impl Damage {
    fn into_poke(self) -> impl Fn() + Send + Sync {
        move || {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state
                .1
                .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(100))
            {
                return;
            }
            state.0 = !state.0;
            state.1 = Some(std::time::Instant::now());
            set_background(if state.0 { "#336698" } else { "#336699" });
        }
    }
}

fn set_background(colour: &str) {
    if std::env::var_os("SWAYSOCK").is_none() {
        return;
    }
    let _status = std::process::Command::new("swaymsg")
        .args(["output", "HEADLESS-1", "bg", colour, "solid_color"])
        .stdout(std::process::Stdio::null())
        .status();
}

#[test]
fn wlr_screencopy_passes_the_checklist_on_headless_sway() {
    if !common::enabled(SWAY) {
        return;
    }
    checklist(&live(), &sway_scene(BackendId::LinuxWlrScreencopy));
}

#[test]
fn ext_image_copy_passes_the_checklist_on_headless_sway() {
    if !common::enabled(SWAY) {
        return;
    }
    checklist(&live(), &sway_scene(BackendId::LinuxExtImageCopy));
    // Leave the known colour for the pixel probes of the other tests.
    set_background("#336699");
    std::thread::sleep(std::time::Duration::from_millis(500));
}

fn pixel(image: &istmo_screen_capture::RawImage, x: u32, y: u32) -> [u8; 3] {
    let at = (y * image.stride + x * 4) as usize;
    [image.bytes[at], image.bytes[at + 1], image.bytes[at + 2]]
}

#[test]
fn monitors_compose_and_turn_on_headless_sway() {
    if !common::enabled(SWAY) {
        return;
    }
    let live = live();
    for backend in [BackendId::LinuxWlrScreencopy, BackendId::LinuxExtImageCopy] {
        let sources = block(live.client.sources(SourceFilter {
            backend: Some(backend),
            ..SourceFilter::default()
        }))
        .expect("sources");
        assert_eq!(sources.sources.len(), 2, "{backend}: {sources:#?}");
        let second = &sources.sources[1];
        // 1280x720 at scale 2, turned 90°: 360x640 logical.
        let geometry = second.geometry.expect("geometry");
        assert_eq!((geometry.width, geometry.height), (360, 640), "{backend}");
        assert_eq!(second.scale_permille, Some(2000));

        let shot = |source| {
            block(live.client.screenshot_cpu(
                ScreenshotRequest::of(source).backend(fixed(backend)),
                PixelLayout::default(),
            ))
            .expect("screenshot")
        };
        let turned = shot(SourceSelector::Id(second.id.clone()));
        assert_eq!(
            (turned.width, turned.height),
            (720, 1280),
            "{backend}: upright size"
        );
        assert_eq!(pixel(&turned, 5, 5), SECOND);

        let all = shot(SourceSelector::AllMonitors);
        // Highest scale (2.0): (1280 + 360) x 720 logical → 3280 x 1440.
        assert_eq!((all.width, all.height), (3280, 1440), "{backend}");
        assert_eq!(pixel(&all, 100, 100), FIRST);
        assert_eq!(pixel(&all, 2600, 1200), SECOND);
        // Below the second output (640 logical high) is a gap.
        let gap = (1400 * all.stride + 2600 * 4) as usize;
        assert_eq!(all.bytes[gap + 3], 0, "the gap is transparent");
        let at = (1435 * all.stride + 100 * 4) as usize;
        assert_eq!(
            all.bytes[at + 3],
            255,
            "first output covers its full height"
        );
    }
}

#[test]
fn niri_streams_dma_bufs_without_copies() {
    if !common::enabled(NIRI) {
        return;
    }
    dma_buf_stream(
        &live(),
        BackendId::LinuxWlrScreencopy,
        SyncKind::Ready,
        &|| {},
    );
}

/// Headless sway renders with pixman by default (`wl_shm` only); with
/// `WLR_RENDERER=gles2` it offers DMA-BUFs (and, on NVIDIA, only a 24-bit
/// `wl_shm` format the CPU tiers do not read).
#[test]
fn sway_gles2_streams_dma_bufs_without_copies() {
    if !common::enabled(SWAY) || std::env::var("WLR_RENDERER").as_deref() != Ok("gles2") {
        return;
    }
    let live = live();
    let poke = Damage::default().into_poke();
    for backend in [BackendId::LinuxWlrScreencopy, BackendId::LinuxExtImageCopy] {
        dma_buf_stream(&live, backend, SyncKind::Implicit, &poke);
    }
    set_background("#336699");
}

fn dma_buf_stream(live: &common::Live, backend: BackendId, sync: SyncKind, poke: &dyn Fn()) {
    let caps = common::caps(live, backend);
    assert!(
        caps.video.iter().any(|f| f.memory == BufferMemory::DmaBuf),
        "{caps:#?}"
    );
    let tiers = FormatRequest {
        tiers: vec![
            FormatTier::new(
                [PixelFormat::Bgrx8, PixelFormat::Bgra8]
                    .into_iter()
                    .map(|f| FormatChoice::dmabuf(f, Vec::new()))
                    .collect(),
            ),
            FormatTier::cpu_rgb(),
        ],
    };
    let config = SessionConfig::default()
        .backend(fixed(backend))
        .fps(FrameRate::Native)
        .formats(tiers);
    let session = block(
        live.client
            .open_session(&live.runtime, config, SinkPolicy::queue(2)),
    )
    .expect("open");
    let negotiated = session.negotiated().clone();
    eprintln!("negotiated: {negotiated:#?}");
    assert_eq!(negotiated.memory, BufferMemory::DmaBuf);
    assert!(
        negotiated.copies.is_empty(),
        "zero-copy: {:?}",
        negotiated.copies
    );
    assert_eq!(negotiated.sync, sync, "{backend}");
    let mut identities = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while identities.len() < 10 && std::time::Instant::now() < deadline {
        poke();
        let Some(frame) = session.video().try_recv() else {
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        };
        assert_eq!(frame.buffer.kind(), MemoryKind::DmaBuf);
        let dmabuf = frame.buffer.dmabuf().expect("DMA-BUF backing");
        for plane in &dmabuf.planes {
            let flags = rustix::io::fcntl_getfd(&plane.fd).expect("fd flags");
            assert!(
                flags.contains(rustix::io::FdFlags::CLOEXEC),
                "{backend}: fd without CLOEXEC"
            );
        }
        assert!(frame.meta.copied.is_none());
        assert!(frame.meta.modifier.is_some());
        identities.push(frame.meta.buffer);
    }
    assert_eq!(identities.len(), 10, "{backend}: ten frames");
    // The first DMA-BUF names its modifier and GPU in the negotiated stream.
    let renegotiated = common::events_of(&session)
        .into_iter()
        .find_map(|e| match e {
            CaptureEvent::Renegotiated(stream) => Some(stream),
            _ => None,
        })
        .expect("Renegotiated with the first frame");
    assert!(renegotiated.modifier.is_some(), "{renegotiated:?}");
    assert!(renegotiated.producer.is_some());
    identities.sort_by_key(|b| b.slot);
    identities.dedup();
    assert!(
        identities.len() <= 4,
        "frames reuse the pool's slots: {identities:?}"
    );
    block(session.close()).expect("close");
    assert!(!negotiated.copies.contains(&CopyStage::ShmTransport));
    dma_buf_readback(live, backend, poke);
}

/// `CopyToCpu` on DMA-BUF: past the hold limit the compositor reads the
/// frame back into `wl_shm`; nothing maps the tiled buffer.
fn dma_buf_readback(live: &common::Live, backend: BackendId, poke: &dyn Fn()) {
    let config = SessionConfig::default()
        .backend(fixed(backend))
        .fps(FrameRate::Native)
        .pool_depth(3)
        .exhaustion(istmo_screen_capture::Exhaustion::CopyToCpu)
        .formats(FormatRequest {
            tiers: vec![FormatTier::new(
                [PixelFormat::Bgrx8, PixelFormat::Bgra8]
                    .into_iter()
                    .map(|f| FormatChoice::dmabuf(f, Vec::new()))
                    .collect(),
            )],
        });
    let session = block(
        live.client
            .open_session(&live.runtime, config, SinkPolicy::queue(64)),
    )
    .expect("DMA-BUF with CopyToCpu");
    let mut held = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while held.len() < 6 && std::time::Instant::now() < deadline {
        poke();
        if let Some(frame) = session.video().try_recv() {
            held.push(frame);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        held.len(),
        6,
        "{backend}: frames flow while every buffer is held"
    );
    assert!(
        held.iter()
            .take(2)
            .all(|f| f.buffer.kind() == MemoryKind::DmaBuf)
    );
    let readback = held
        .iter()
        .find(|f| f.meta.copied == Some(CopyStage::ExhaustionReadback))
        .expect("read-back frames past the limit");
    assert_eq!(readback.buffer.kind(), MemoryKind::Cpu);
    assert_eq!(readback.meta.memory, BufferMemory::Cpu);
    drop(held);
    block(session.close()).expect("close");
}

#[test]
fn niri_passes_the_checklist() {
    if !common::enabled(NIRI) {
        return;
    }
    checklist(
        &live(),
        &Scene {
            backend: BackendId::LinuxWlrScreencopy,
            probes: Vec::new(),
            poke: Box::new(|| {}),
        },
    );
}

fn swaymsg(args: &[&str]) {
    let status = std::process::Command::new("swaymsg")
        .args(args)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("swaymsg runs");
    assert!(status.success(), "swaymsg {args:?}");
}

#[test]
fn an_unplugged_output_ends_the_session_with_source_lost() {
    if !common::enabled(SWAY) {
        return;
    }
    let live = live();
    for backend in [BackendId::LinuxWlrScreencopy, BackendId::LinuxExtImageCopy] {
        swaymsg(&["create_output"]);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let sources = block(live.client.sources(SourceFilter {
            backend: Some(backend),
            ..SourceFilter::default()
        }))
        .expect("sources");
        let extra = sources
            .sources
            .iter()
            .find(|s| s.id.value != "HEADLESS-1" && s.id.value != "HEADLESS-2")
            .expect("the created output")
            .clone();
        let config = SessionConfig::monitor(SourceSelector::Id(extra.id.clone()))
            .backend(fixed(backend))
            .fps(FrameRate::Native);
        let session = block(
            live.client
                .open_session(&live.runtime, config, SinkPolicy::latest()),
        )
        .expect("open");
        let _first = session.video().recv().expect("a frame");
        swaymsg(&["output", &extra.id.value, "unplug"]);
        // Drain until the sink ends.
        while session.video().recv().is_some() {}
        let reason = session.video().end_reason().expect("ended");
        let error = reason.error().map(ToString::to_string).unwrap_or_default();
        assert!(error.contains("went away"), "{backend}: {reason:?}");
    }
}
