#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
)))]
fn main() -> Result<(), eframe::Error> {
    use std::sync::Arc;

    use pen_demo::{DrawApp, SharedInk};

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let ink = Arc::new(SharedInk::default());

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([960.0, 640.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "istmo-pen demo",
        options,
        Box::new(move |_cc| Ok(Box::new(DrawApp::new(ink)))),
    )
}

#[cfg(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
))]
fn main() {}
