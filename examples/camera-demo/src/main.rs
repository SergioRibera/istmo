//! Desktop entry point. Spin up the runtime, register the pure-Rust
//! `DesktopCamera` host, then hand the client to eframe + egui.

#![cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let client = pollster::block_on(camera_demo::bootstrap())?;

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([1024.0, 720.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "istmo-camera demo",
        options,
        Box::new(move |_cc| Ok(Box::new(camera_demo::CameraApp::new(client)))),
    )?;
    Ok(())
}
