#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
)))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::Arc;

    use istmo_core::{Runtime, RuntimeConfig, RuntimeInit};
    use istmo_pen::backend::PenPublisherFactory;
    use istmo_pen::publisher::PenPublisher;
    use istmo_pen::{PenClient, PenConfig, PenHost};
    use pen_demo::{DrawApp, SharedInk, WINDOW_ID};

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let ink = Arc::new(SharedInk::default());

    let RuntimeInit { runtime, outbound } = Runtime::init(RuntimeConfig::inline())?;

    let publisher = PenPublisher::install(&runtime);
    publisher.register_window_id(WINDOW_ID);

    #[cfg(target_os = "linux")]
    if let Err(err) = publisher.install_libinput() {
        log::warn!(
            "libinput backend unavailable ({err}); pen-demo falls back to egui pointer events. \
             Real stylus samples on Wayland/X11 require compositor tablet-protocol integration."
        );
    }

    runtime.register_host(PenHost::new(PenPublisherFactory::new(Arc::clone(&publisher))));

    // Drain outbound so bounded channel never backs up. Local-hosted
    // Call frames short-circuit into dispatch_inbound, so only
    // ReleaseNativeHandle / Notify frames ever land here — none in this
    // demo, but we drain defensively.
    std::thread::spawn(move || while outbound.recv().is_ok() {});

    let client = pollster::block_on(PenClient::acquire_with(PenConfig::new(WINDOW_ID)))?;
    let events = client.events()?;
    let hover = client.hover()?;

    let ink_events = Arc::clone(&ink);
    std::thread::spawn(move || pen_demo::pump_events_stream(events, ink_events));
    let ink_hover = Arc::clone(&ink);
    std::thread::spawn(move || pen_demo::pump_hover_stream(hover, ink_hover));

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([960.0, 640.0]),
        ..eframe::NativeOptions::default()
    };
    let ink_for_app = Arc::clone(&ink);
    let publisher_for_app = Arc::clone(&publisher);
    eframe::run_native(
        "istmo-pen demo",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(DrawApp::new_desktop(
                ink_for_app,
                publisher_for_app,
            )))
        }),
    )?;
    // Keep the client alive for the lifetime of eframe.
    drop(client);
    Ok(())
}

#[cfg(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
))]
fn main() {}
