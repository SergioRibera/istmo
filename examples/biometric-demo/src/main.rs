#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
)))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use biometric_demo::app::{DemoApp, SharedState};
    use istmo_biometric::{BiometricClient, BiometricHost, DesktopBiometric};
    use istmo_core::{Runtime, RuntimeConfig, RuntimeInit};

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let RuntimeInit { runtime, outbound } = Runtime::init(RuntimeConfig::inline())?;
    // Locally hosted calls short-circuit into `dispatch_inbound`; drain
    // the outbound queue defensively so it never backs up.
    std::thread::spawn(move || while outbound.recv().is_ok() {});

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([560.0, 640.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "istmo biometric demo",
        options,
        Box::new(move |cc| {
            // Parenting the prompt to the window keeps Windows Hello in
            // front of the app.
            runtime.register_host(BiometricHost::new(
                DesktopBiometric::default().with_parent_window(cc),
            ));
            let client = BiometricClient::from_runtime(&runtime)?;
            Ok(Box::new(DemoApp::new(SharedState::new(
                client,
                cc.egui_ctx.clone(),
            ))))
        }),
    )?;
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
