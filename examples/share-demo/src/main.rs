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
    use istmo_share::{DesktopShare, ShareClient, ShareHost, ShareInbox};
    use istmo_window::{WindowId, WindowRegistry};
    use share_demo::app::{DemoApp, SharedState};

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let RuntimeInit { runtime, outbound } = Runtime::init(RuntimeConfig::inline())?;
    // Locally hosted calls short-circuit into `dispatch_inbound`; drain
    // the outbound queue defensively so it never backs up.
    std::thread::spawn(move || while outbound.recv().is_ok() {});

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([560.0, 760.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "istmo share demo",
        options,
        Box::new(move |cc| {
            // The share UI anchors on this window (Windows / macOS).
            let registry = Arc::clone(WindowRegistry::global());
            registry.register(WindowId(1), cc)?;
            runtime.register_host(ShareHost::new(DesktopShare::new(registry)?));
            let client = ShareClient::from_runtime(&runtime)?;
            let inbox = ShareInbox::from_runtime(&runtime)?;
            Ok(Box::new(DemoApp::new(SharedState::new(
                client,
                inbox,
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
