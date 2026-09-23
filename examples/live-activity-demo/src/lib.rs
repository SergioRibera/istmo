pub mod app;

#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
)))]
pub mod emulator;

use istmo_live_activity::LiveActivityClient;

istmo::runtime!(
    plugins: [LiveActivityClient],
);

#[cfg(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
))]
#[istmo::mobile_app]
fn mobile_main() {
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    log::info!("live-activity-demo mobile entry point running");

    let runtime = istmo::runtime();
    let activities = match app::activities(&runtime) {
        Ok(a) => a,
        Err(err) => {
            log::error!("bind TypedLiveActivity failed: {err:?}");
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
        "live-activity-demo",
        options,
        Box::new(move |cc| {
            let shared =
                std::sync::Arc::new(app::SharedState::new(activities, cc.egui_ctx.clone()));
            Ok(Box::new(app::DemoApp::new(shared)))
        }),
    ) {
        log::error!("eframe exited with error: {err:?}");
    }
}
