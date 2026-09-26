//! share-demo shared entry — the desktop `main.rs` and the mobile
//! `#[istmo::mobile_app]` entry both run the same [`app::DemoApp`].

pub mod app;
mod png;

use istmo_share::ShareClient;

istmo::runtime!(
    plugins: [ShareClient],
);

#[cfg(any(target_os = "android", target_os = "ios"))]
#[istmo::mobile_app]
fn mobile_main() {
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    log::info!("share-demo mobile entry point running");

    // The Kotlin / Swift side registered `ShareDispatcher` before handing
    // control to Rust.
    let client = match ShareClient::acquire() {
        Ok(client) => client,
        Err(err) => {
            log::error!("acquire ShareClient failed: {err:?}");
            return;
        }
    };
    // Shares that cold-started the app were buffered until now.
    let inbox = match istmo_share::ShareInbox::acquire() {
        Ok(inbox) => inbox,
        Err(err) => {
            log::error!("acquire ShareInbox failed: {err:?}");
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
        "share-demo",
        options,
        Box::new(move |cc| {
            let shared = app::SharedState::new(client, inbox, cc.egui_ctx.clone());
            Ok(Box::new(app::DemoApp::new(shared)))
        }),
    ) {
        log::error!("eframe exited with error: {err:?}");
    }
}
