//! pen-demo shared entry — desktop `main.rs` and Android
//! `#[istmo::mobile_app]` both wind up here to spin up the `DrawApp`
//! eframe surface.

pub mod app;

pub const WINDOW_ID: u64 = 1;

pub use app::{DrawApp, PenDebug, SharedInk, Stroke, StrokePoint};

use istmo::plugins::SafeArea;
use istmo_pen::{PenClient, PenEvent, PenHoverEvent, PenSample, PenToolKind};

istmo::runtime!(
    plugins: [PenClient, SafeArea],
);

#[cfg(target_os = "android")]
use istmo_pen::PenConfig;

/// Drain the [`PenClient::events`] stream — decoding each `PenEvent`
/// into a `StrokePoint` that the drawing app renders. Blocking; the
/// mobile entry spawns this off before it hands the thread to eframe.
pub fn pump_events_stream(
    stream: istmo_core::TypedStream<PenEvent, ()>,
    ink: std::sync::Arc<SharedInk>,
) {
    loop {
        match stream.recv() {
            Ok(istmo_core::StreamItem::Event(event)) => apply_event(event, &ink),
            Ok(istmo_core::StreamItem::Completed | istmo_core::StreamItem::Cancelled) => break,
            Ok(istmo_core::StreamItem::Failed(_)) => break,
            Err(err) => {
                log::error!("pen events recv error: {err:?}");
                break;
            }
        }
    }
}

pub fn pump_hover_stream(
    stream: istmo_core::TypedStream<PenHoverEvent, ()>,
    _ink: std::sync::Arc<SharedInk>,
) {
    // Hover samples aren't drawn in this demo, but we still drain the
    // stream so the wire buffer doesn't fill and the plugin backend
    // can log activity.
    loop {
        match stream.recv() {
            Ok(istmo_core::StreamItem::Event(_)) => {}
            Ok(istmo_core::StreamItem::Completed | istmo_core::StreamItem::Cancelled) => break,
            Ok(istmo_core::StreamItem::Failed(_)) => break,
            Err(_) => break,
        }
    }
}

fn apply_event(event: PenEvent, ink: &std::sync::Arc<SharedInk>) {
    match event {
        PenEvent::Down(ref sample) => {
            ink.begin(point_from_sample(sample));
            publish_debug(ink, sample, "down");
        }
        PenEvent::Move(ref m) => {
            for c in &m.coalesced {
                ink.extend(point_from_sample(c));
            }
            ink.extend(point_from_sample(&m.sample));
            publish_debug(ink, &m.sample, "move");
        }
        PenEvent::Up(ref sample) => {
            ink.extend(point_from_sample(sample));
            ink.end();
            publish_debug(ink, sample, "up");
        }
        PenEvent::Cancel(ref sample) => {
            ink.end();
            publish_debug(ink, sample, "cancel");
        }
        PenEvent::ButtonChanged(ref change) => {
            publish_debug(ink, &change.sample, "button");
            // Toggle the color menu whenever barrel button 1 flips.
            if change.changed & 1 != 0 && change.sample.buttons & 1 != 0 {
                ink.toggle_color_menu();
            }
        }
    }
}

fn publish_debug(ink: &std::sync::Arc<SharedInk>, sample: &PenSample, event: &'static str) {
    ink.update_debug(PenDebug {
        x: sample.x,
        y: sample.y,
        pressure: sample.pressure,
        tilt_x: sample.tilt_x,
        tilt_y: sample.tilt_y,
        azimuth: sample.azimuth,
        altitude: sample.altitude,
        twist: sample.twist,
        tangential_pressure: sample.tangential_pressure,
        z_offset: sample.z_offset,
        timestamp_us: sample.timestamp_us,
        sequence: sample.sequence,
        tool_id: sample.tool_id,
        tool_kind: match sample.tool_kind {
            PenToolKind::Tip => "tip",
            PenToolKind::Eraser => "eraser",
            PenToolKind::Unknown => "unknown",
        },
        buttons: sample.buttons,
        last_event: event,
        touches: 0,
    });
}

fn point_from_sample(sample: &PenSample) -> StrokePoint {
    StrokePoint {
        pos: eframe::egui::Pos2::new(sample.x, sample.y),
        pressure: sample.pressure.clamp(0.0, 1.0),
        tilt: (sample.tilt_x.powi(2) + sample.tilt_y.powi(2)).sqrt(),
    }
}

#[cfg(target_os = "android")]
#[istmo::mobile_app]
fn mobile_main() {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    log::info!("pen-demo mobile_main");

    let ink = std::sync::Arc::new(SharedInk::default());

    // Kick off the PenClient drain on a background thread so
    // android_main hands off cleanly to eframe / winit and the
    // NativeActivity surface finishes configuring.
    let ink_bg = std::sync::Arc::clone(&ink);
    std::thread::spawn(move || {
        let client = match pollster::block_on(PenClient::acquire_with(PenConfig::new(WINDOW_ID))) {
            Ok(c) => c,
            Err(err) => {
                log::error!("PenClient::acquire_with failed: {err:?}");
                return;
            }
        };
        let events = match client.events() {
            Ok(s) => s,
            Err(err) => {
                log::error!("events stream failed: {err:?}");
                return;
            }
        };
        let hover = match client.hover() {
            Ok(s) => s,
            Err(err) => {
                log::error!("hover stream failed: {err:?}");
                return;
            }
        };
        let hover_ink = std::sync::Arc::clone(&ink_bg);
        std::thread::spawn(move || pump_hover_stream(hover, hover_ink));
        pump_events_stream(events, ink_bg);
    });

    let android_app = istmo::android::android_app()
        .expect("android_main should have stashed the AndroidApp handle");
    let event_loop_builder: Option<
        Box<dyn FnOnce(&mut winit::event_loop::EventLoopBuilder<eframe::UserEvent>)>,
    > = Some(Box::new(move |builder| {
        use winit::platform::android::EventLoopBuilderExtAndroid;
        builder.with_android_app(android_app);
    }));

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        event_loop_builder,
        ..Default::default()
    };

    if let Err(err) = eframe::run_native(
        "pen-demo",
        options,
        Box::new(move |_cc| Ok(Box::new(DrawApp::new(ink)))),
    ) {
        log::error!("eframe exited: {err:?}");
    }
}
