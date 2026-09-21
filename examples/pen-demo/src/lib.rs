//! pen-demo shared entry — desktop `main.rs` and Android
//! `#[istmo::mobile_app]` both wind up here to spin up the `DrawApp`
//! eframe surface.

pub mod app;

pub const WINDOW_ID: u64 = 1;

pub use app::{DrawApp, SharedInk, Stroke, StrokePoint};

use istmo_pen::{PenClient, PenEvent, PenHoverEvent};

istmo::runtime!(
    plugins: [PenClient],
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
        PenEvent::Down(sample) => ink.begin(point_from_sample(&sample)),
        PenEvent::Move(m) => {
            for c in &m.coalesced {
                ink.extend(point_from_sample(c));
            }
            ink.extend(point_from_sample(&m.sample));
        }
        PenEvent::Up(sample) => {
            ink.extend(point_from_sample(&sample));
            ink.end();
        }
        PenEvent::Cancel(_) => ink.end(),
        PenEvent::ButtonChanged(_) => {}
    }
}

fn point_from_sample(sample: &istmo_pen::PenSample) -> StrokePoint {
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
