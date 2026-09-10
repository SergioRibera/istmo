//! Full-multiplatform istmo demo — one Rust crate driving desktop, Android
//! and iOS clients of the `istmo.data_store` plugin.
//!
//! Layout:
//!
//! * `app` — shared eframe UI + `DemoApp` state.
//! * `emulator` — desktop-only Rust-side "native emulator" that stands in
//!   for the Kotlin / Swift dispatcher when the demo runs on a laptop.
//! * `main.rs` (bin) — desktop entrypoint that boots the emulator and
//!   `DataStoreClient::from_runtime_with(...)`.
//! * `lib.rs` (this file) — mobile transport wiring. `istmo::runtime!`
//!   installs the JNI trampolines on Android and the `@_cdecl` symbols on
//!   iOS; `#[istmo::mobile_app]` emits `android_main` / `istmo_run_ios`
//!   entry points that boot the shared `DemoApp` after
//!   `DataStoreClient::acquire_with(config)` resolves against the
//!   process-global `Runtime`.
//!
//! Zero `#[cfg(target_os = ...)]` in `app`. The only per-platform code
//! lives in `main.rs` (desktop-only boot) and the two branches below
//! (`AndroidApp` handle threading on Android).

pub mod app;

#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
)))]
pub mod emulator;

// `DataStoreClient` is imported by `istmo::runtime!` below through the
// `plugins:` list.
use istmo_data_store::DataStoreClient;

istmo::runtime!(
    plugins: [DataStoreClient],
);

/// Mobile entrypoint. `#[istmo::mobile_app]` re-exports this as
/// `android_main` on Android and `istmo_run_ios` on the Apple mobile
/// family; on desktop the attribute is a no-op and `main.rs` calls the
/// shared app entry directly.
#[cfg(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
))]
#[istmo::mobile_app]
fn mobile_main() {
    use istmo_data_store::DataStoreConfig;

    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    log::info!("data-store-demo mobile entry point running");

    let cfg = DataStoreConfig::new(app::namespace());
    let client = match pollster::block_on(DataStoreClient::acquire_with(cfg)) {
        Ok(c) => c,
        Err(err) => {
            log::error!("acquire DataStoreClient failed: {err:?}");
            return;
        }
    };

    #[cfg(target_os = "android")]
    let event_loop_builder: Option<
        Box<dyn FnOnce(&mut winit::event_loop::EventLoopBuilder<eframe::UserEvent>)>,
    > = {
        let android_app = istmo::android::android_app()
            .expect("android_main should have stashed the AndroidApp handle before entering main");
        Some(Box::new(move |builder| {
            use winit::platform::android::EventLoopBuilderExtAndroid;
            builder.with_android_app(android_app);
        }))
    };
    #[cfg(not(target_os = "android"))]
    let event_loop_builder: Option<
        Box<dyn FnOnce(&mut winit::event_loop::EventLoopBuilder<eframe::UserEvent>)>,
    > = None;

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        event_loop_builder,
        ..Default::default()
    };

    if let Err(err) = eframe::run_native(
        "data-store-demo",
        options,
        Box::new(move |cc| {
            let shared = std::sync::Arc::new(app::SharedState {
                client,
                egui_ctx: cc.egui_ctx.clone(),
                transcript: std::sync::Mutex::new(Vec::new()),
                keys_snapshot: std::sync::Mutex::new(Vec::new()),
            });
            Ok(Box::new(app::DemoApp::new(shared)))
        }),
    ) {
        log::error!("eframe exited with error: {err:?}");
    }
}
