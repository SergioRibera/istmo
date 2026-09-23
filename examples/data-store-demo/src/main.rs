#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
)))]
fn main() -> Result<(), eframe::Error> {
    use std::sync::{Arc, Mutex};

    use data_store_demo::app::{self, DemoApp, SharedState};
    use data_store_demo::emulator;
    use istmo_core::{Runtime, RuntimeInit};
    use istmo_data_store::{DataStoreClient, DataStoreConfig};

    let RuntimeInit { runtime, outbound } = Runtime::mock();
    emulator::spawn(Arc::clone(&runtime), outbound);

    let config = DataStoreConfig::new(app::namespace());
    let client = pollster::block_on(DataStoreClient::from_runtime_with(&runtime, config))
        .expect("acquire DataStoreClient");

    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([720.0, 520.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "istmo data-store demo",
        opts,
        Box::new(move |cc| {
            let shared = Arc::new(SharedState {
                client,
                egui_ctx: cc.egui_ctx.clone(),
                transcript: Mutex::new(Vec::new()),
                keys_snapshot: Mutex::new(Vec::new()),
            });
            Ok(Box::new(DemoApp::new(shared)))
        }),
    )
}

#[cfg(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos",
    target_os = "watchos",
))]
fn main() {}
