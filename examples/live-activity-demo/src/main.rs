//! Desktop entrypoint. On mobile targets this compiles to an unused stub
//! `fn main() {}` — the cdylib/staticlib carries the real entry via
//! `#[istmo::mobile_app]` in `lib.rs`.

#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
)))]
fn main() -> Result<(), eframe::Error> {
    use std::sync::Arc;

    use istmo_core::{Runtime, RuntimeInit};
    use live_activity_demo::app::{self, DemoApp, SharedState};
    use live_activity_demo::emulator;

    let RuntimeInit { runtime, outbound } = Runtime::mock();
    emulator::spawn(Arc::clone(&runtime), outbound);

    let activities = app::activities(&runtime).expect("bind TypedLiveActivity");

    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([720.0, 640.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "istmo live-activity demo",
        opts,
        Box::new(move |cc| {
            let shared = Arc::new(SharedState::new(activities, cc.egui_ctx.clone()));
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
fn main() {
    // Mobile targets carry the entrypoint in the cdylib / staticlib
    // (`#[istmo::mobile_app]` in `lib.rs`). Keep this stub around so
    // cargo's default `bin` target still compiles.
}
