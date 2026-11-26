//! The X11 backend under Xvfb: the per-backend checklist plus pixel
//! checks of crops, composites and windows. Run by
//! `scripts/test-screen-capture-x11.sh` (two `RandR` monitors, A at
//! 0..320 and B at 320..640 of a 640x480 screen); skipped elsewhere.

#![cfg(all(target_os = "linux", feature = "linux-x11"))]

mod common;

use std::sync::Arc;
use std::time::Duration;

use istmo_core::SinkPolicy;
use istmo_screen_capture::{
    BackendId, CompositeConfig, CompositeScale, Damage, FrameRate, PixelLayout, Rect,
    RegionSelector, RegionSpace, ScreenCaptureClientExt, ScreenshotRequest, SessionConfig,
    SourceFilter, SourceKind, SourceSelector,
};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConnectionExt as _, CreateWindowAux, Window, WindowClass};
use x11rb::rust_connection::RustConnection;

use common::{Probe, Scene, block, checklist, enabled, fixed, live, wait_until};

const GATE: &str = "ISTMO_SC_TEST_X11";
const X11: BackendId = BackendId::LinuxX11;

const RED: u32 = 0x00ff_0000;
const BLUE: u32 = 0x0000_00ff;
const GREEN: u32 = 0x0000_ff00;
const CYAN: u32 = 0x0000_ffff;
const YELLOW: u32 = 0x00ff_ff00;

/// The painted scene: one connection holding override-redirect windows
/// with solid backgrounds (the server paints them, no client drawing).
struct Painter {
    conn: RustConnection,
    root: Window,
    green: Window,
}

impl Painter {
    fn new() -> Self {
        let (conn, screen) = RustConnection::connect(None).expect("connect to Xvfb");
        let root = conn.setup().roots[screen].root;
        let mut painter = Self {
            conn,
            root,
            green: 0,
        };
        painter.split_monitors();
        painter.window(Rect::new(0, 0, 320, 480), RED);
        painter.window(Rect::new(320, 0, 320, 480), BLUE);
        painter.green = painter.window(Rect::new(100, 100, 40, 40), GREEN);
        painter.sync();
        painter
    }

    /// Two `RandR` monitors, A (left half) and B (right half), in place
    /// of the screen's own.
    fn split_monitors(&self) {
        use x11rb::protocol::randr::{ConnectionExt as _, MonitorInfo};
        let atom = |name: &[u8]| {
            self.conn
                .intern_atom(false, name)
                .expect("atom")
                .reply()
                .expect("atom")
                .atom
        };
        for (name, x) in [(&b"A"[..], 0), (&b"B"[..], 320)] {
            let monitor = MonitorInfo {
                name: atom(name),
                primary: x == 0,
                automatic: false,
                x,
                y: 0,
                width: 320,
                height: 480,
                width_in_millimeters: 85,
                height_in_millimeters: 127,
                outputs: Vec::new(),
            };
            self.conn
                .randr_set_monitor(self.root, monitor)
                .expect("set monitor")
                .check()
                .expect("set monitor");
        }
        let monitors = self
            .conn
            .randr_get_monitors(self.root, false)
            .expect("monitors")
            .reply()
            .expect("monitors");
        assert!(monitors.monitors.len() >= 2, "{:?}", monitors.monitors);
    }

    fn window(&self, rect: Rect, colour: u32) -> Window {
        let id = self.conn.generate_id().expect("id");
        let aux = CreateWindowAux::new()
            .background_pixel(colour)
            .override_redirect(1);
        self.conn
            .create_window(
                0,
                id,
                self.root,
                i16::try_from(rect.x).expect("fits"),
                i16::try_from(rect.y).expect("fits"),
                u16::try_from(rect.width).expect("fits"),
                u16::try_from(rect.height).expect("fits"),
                0,
                WindowClass::INPUT_OUTPUT,
                0,
                &aux,
            )
            .expect("create window");
        self.conn.map_window(id).expect("map");
        id
    }

    /// Repaint the green square with the same colour: damage, same pixels.
    fn poke(&self) {
        let _sent = self.conn.clear_area(false, self.green, 0, 0, 0, 0);
        let _flushed = self.conn.flush();
    }

    fn sync(&self) {
        self.conn
            .get_input_focus()
            .expect("sync")
            .reply()
            .expect("sync");
        // Let the server paint the mapped windows.
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn rgb(image: &istmo_screen_capture::RawImage, x: u32, y: u32) -> [u8; 3] {
    let at = (y * image.stride + x * 4) as usize;
    [image.bytes[at], image.bytes[at + 1], image.bytes[at + 2]]
}

fn shot(request: ScreenshotRequest) -> istmo_screen_capture::RawImage {
    let live = live();
    block(
        live.client
            .screenshot_cpu(request.backend(fixed(X11)), PixelLayout::default()),
    )
    .expect("screenshot")
}

#[test]
#[expect(clippy::too_many_lines, reason = "one Xvfb server, every check in sequence")]
fn x11_under_xvfb() {
    if !enabled(GATE) {
        return;
    }
    let painter = Arc::new(Painter::new());
    let live = live();

    // Two RandR monitors, no XWayland, so X11 is ready and in Auto.
    let list = block(live.client.sources(SourceFilter::default())).expect("sources");
    // Xvfb keeps its automatic whole-screen monitor next to A and B.
    let monitor = |name: &str| {
        list.sources
            .iter()
            .find(|s| s.kind == SourceKind::Monitor && s.id.value == format!("monitor:{name}"))
            .cloned()
            .unwrap_or_else(|| panic!("monitor {name} in {:#?}", list.sources))
    };
    let (a, b) = (monitor("A"), monitor("B"));
    assert!(list.sources.iter().any(|s| s.kind == SourceKind::Window));

    let poker = Arc::clone(&painter);
    checklist(
        &live,
        &Scene {
            backend: X11,
            probes: vec![
                Probe {
                    x: 10,
                    y: 10,
                    rgb: [255, 0, 0],
                },
                Probe {
                    x: 110,
                    y: 110,
                    rgb: [0, 255, 0],
                },
            ],
            poke: Box::new(move || poker.poke()),
        },
    );

    // Auto picks X11 on an X11 session.
    let image = block(
        live.client
            .screenshot_cpu(ScreenshotRequest::default(), PixelLayout::default()),
    )
    .expect("auto screenshot");
    assert_eq!(image.height, 480);

    // Every monitor composed into one image.
    let all = shot(ScreenshotRequest::of(SourceSelector::Monitors(vec![
        a.id.clone(),
        b.id,
    ])));
    assert_eq!((all.width, all.height), (640, 480));
    assert_eq!(rgb(&all, 10, 10), [255, 0, 0]);
    assert_eq!(rgb(&all, 330, 10), [0, 0, 255]);
    assert_eq!(rgb(&all, 120, 120), [0, 255, 0]);

    // A region inside one monitor is a crop; across both a composite.
    let inside = shot(ScreenshotRequest::of(SourceSelector::Region(
        RegionSelector {
            rect: Rect::new(100, 100, 40, 40),
            space: RegionSpace::Global,
        },
    )));
    assert_eq!((inside.width, inside.height), (40, 40));
    assert_eq!(rgb(&inside, 0, 0), [0, 255, 0]);
    assert_eq!(rgb(&inside, 39, 39), [0, 255, 0]);
    let across = shot(ScreenshotRequest {
        composite_config: CompositeConfig {
            scale: CompositeScale::Logical,
            ..CompositeConfig::default()
        },
        ..ScreenshotRequest::of(SourceSelector::Region(RegionSelector {
            rect: Rect::new(300, 0, 40, 10),
            space: RegionSpace::Global,
        }))
    });
    assert_eq!((across.width, across.height), (40, 10));
    assert_eq!(rgb(&across, 0, 0), [255, 0, 0]);
    assert_eq!(rgb(&across, 39, 9), [0, 0, 255]);

    // Damage rectangles name the repainted square (with XDamage).
    if common::caps(&live, X11).damage == istmo_screen_capture::DamageSupport::Rects {
        let config = SessionConfig::default()
            .backend(fixed(X11))
            .fps(FrameRate::Native);
        let session = block(
            live.client
                .open_session(&live.runtime, config, SinkPolicy::queue(8)),
        )
        .expect("open");
        let mut found = false;
        let ok = wait_until("damage rects", Duration::from_secs(5), || {
            painter.poke();
            while let Some(frame) = session.video().try_recv() {
                if let Damage::Rects(rects) = &frame.meta.damage
                    && rects.contains(&Rect::new(100, 100, 40, 40))
                {
                    found = true;
                }
            }
            found
        });
        assert!(ok, "a frame carries the poked square as damage");
    }

    // A window, partly covered by another: Composite reads it whole.
    let target = painter.window(Rect::new(200, 300, 64, 48), CYAN);
    let _cover = painter.window(Rect::new(210, 310, 20, 20), YELLOW);
    painter.sync();
    let id = istmo_screen_capture::SourceId::new(X11, format!("window:{target:#x}"));
    let window = shot(ScreenshotRequest::of(SourceSelector::Id(id.clone())));
    assert_eq!((window.width, window.height), (64, 48));
    assert_eq!(rgb(&window, 1, 1), [0, 255, 255]);
    assert_eq!(
        rgb(&window, 15, 15),
        [0, 255, 255],
        "occluded content through Composite"
    );

    // The window goes away mid-stream: the sink ends with the reason.
    {
        let config = SessionConfig::monitor(SourceSelector::Id(id))
            .backend(fixed(X11))
            .fps(FrameRate::Native);
        let session = block(
            live.client
                .open_session(&live.runtime, config, SinkPolicy::queue(4)),
        )
        .expect("open window");
        assert!(session.video().recv().is_some(), "a first frame");
        painter.conn.destroy_window(target).expect("destroy");
        painter.conn.flush().expect("flush");
        let ended = wait_until("the source lost", Duration::from_secs(5), || {
            while session.video().try_recv().is_some() {}
            session.video().end_reason().is_some()
        });
        assert!(ended);
        let reason = session.video().end_reason().expect("ended");
        assert!(reason.error().is_some(), "{reason:?}");
    }

    // Focused / under-cursor resolve to real sources.
    let under = shot(ScreenshotRequest::of(SourceSelector::UnderCursor));
    assert_eq!(under.height, 480);
    let monitors_of_a = shot(ScreenshotRequest::of(SourceSelector::Id(a.id)));
    assert_eq!((monitors_of_a.width, monitors_of_a.height), (320, 480));
}
