//! Shared library crate so `main.rs` (native), the wasm entry point,
//! and the Android `#[istmo::mobile_app]` entry can all pull in
//! [`CameraApp`] without duplicating code.

#![doc(html_root_url = "https://docs.rs/camera-demo")]

mod app;

pub use app::CameraApp;

use std::sync::Arc;

use istmo_camera::{CameraClient, CameraInitConfig};

// On Android/iOS the facade's `istmo::runtime!` macro bootstraps the
// runtime + auto-wires every plugin declared through `istmo.toml`.
// Desktop/web stays on the hand-rolled bootstrap below.
#[cfg(any(target_os = "android", target_os = "ios"))]
istmo::runtime!(
    plugins: [istmo_camera::CameraClient],
);

#[cfg(not(any(target_os = "android", target_os = "ios")))]
use istmo_core::{Runtime, RuntimeConfig, RuntimeInit};

/// Build the runtime (desktop + web only), register the correct host
/// for the target, and return a ready-to-call [`CameraClient`]
/// wrapped in `Arc` so it can be cloned into every open session.
///
/// # Errors
/// Returns the runtime bring-up error. The platform-specific host
/// registration is infallible.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub async fn bootstrap() -> Result<Arc<CameraClient>, Box<dyn std::error::Error>> {
    let RuntimeInit { runtime, outbound } = Runtime::init(RuntimeConfig::inline())?;
    register_host(&runtime);

    // Drain outbound so bounded channel never backs up — Rust-hosted
    // Call frames short-circuit into dispatch_inbound, so only
    // ReleaseNativeHandle / Notify frames ever reach the pump. None
    // in this demo, but drain defensively.
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::spawn(move || while outbound.recv().is_ok() {});
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        while outbound.recv_async().await.is_ok() {}
    });

    let client = CameraClient::acquire_with(CameraInitConfig::default()).await?;
    Ok(Arc::new(client))
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn register_host(runtime: &Arc<istmo_core::Runtime>) {
    use istmo_camera::{CameraHost, DesktopCameraFactory};
    runtime.register_host(CameraHost::new(DesktopCameraFactory));
}

#[cfg(target_arch = "wasm32")]
fn register_host(runtime: &Arc<istmo_core::Runtime>) {
    use istmo_camera::{CameraHost, WebCameraFactory};
    runtime.register_host(CameraHost::new(WebCameraFactory));
}

// -------------------------------------------------------------------
// wasm32 entry point — eframe mounts into the `<canvas id="camera">`
// element the demo's `index.html` provides.
// -------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn wasm_start() {
    console_error_panic_hook::set_once();
    console_log::init_with_level(log::Level::Info).ok();

    wasm_bindgen_futures::spawn_local(async {
        let client = match bootstrap().await {
            Ok(c) => c,
            Err(err) => {
                log::error!("camera-demo bootstrap failed: {err}");
                return;
            }
        };

        let web_options = eframe::WebOptions::default();
        let document = web_sys::window()
            .and_then(|w| w.document())
            .expect("document missing on window");
        let canvas = document
            .get_element_by_id("camera")
            .and_then(|el| el.dyn_into::<web_sys::HtmlCanvasElement>().ok())
            .expect("expected <canvas id=\"camera\"> in index.html");

        let runner = eframe::WebRunner::new();
        if let Err(err) = runner
            .start(
                canvas,
                web_options,
                Box::new(move |_cc| Ok(Box::new(CameraApp::new(client)))),
            )
            .await
        {
            log::error!("eframe::WebRunner failed: {err:?}");
        }
    });
}

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

// -------------------------------------------------------------------
// Android entry — the facade's `#[istmo::mobile_app]` macro expands
// the ndk `android_main` entry point around this fn. `istmo::runtime!`
// (above) auto-registered the `CameraClient`; here we hand the client
// to eframe through winit's android-game-activity path.
// -------------------------------------------------------------------

#[cfg(target_os = "android")]
#[istmo::mobile_app]
fn mobile_main() {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    log::info!("camera-demo mobile_main");

    // Resolve the client on a background thread so the main thread
    // can hand off to eframe immediately. Any `block_on` inside the
    // app loop then hits a hot runtime.
    let client_slot: Arc<std::sync::Mutex<Option<Arc<CameraClient>>>> =
        Arc::new(std::sync::Mutex::new(None));
    let client_slot_bg = Arc::clone(&client_slot);
    std::thread::spawn(move || {
        match pollster::block_on(CameraClient::acquire_with(CameraInitConfig::default())) {
            Ok(client) => {
                *client_slot_bg.lock().expect("client slot") = Some(Arc::new(client));
            }
            Err(err) => log::error!("CameraClient::acquire_with failed: {err:?}"),
        }
    });

    let android_app = istmo::android::android_app()
        .expect("android_main should have stashed the AndroidApp handle");
    let event_loop_builder: Option<
        Box<dyn FnOnce(&mut winit::event_loop::EventLoopBuilder<eframe::UserEvent>)>,
    > = Some(Box::new(move |builder| {
        use winit::platform::android::EventLoopBuilderExtAndroid;
        builder.with_android_app(android_app);
    }));

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        event_loop_builder,
        ..Default::default()
    };

    if let Err(err) = eframe::run_native(
        "camera-demo",
        options,
        Box::new(move |_cc| {
            // Spin briefly until the background thread finishes
            // `acquire_with`. On a cold boot the handshake is a few
            // hundred ms; on warm starts it's immediate.
            let client = loop {
                if let Some(c) = client_slot.lock().expect("client slot").clone() {
                    break c;
                }
                std::thread::sleep(std::time::Duration::from_millis(16));
            };
            Ok(Box::new(CameraApp::new(client)))
        }),
    ) {
        log::error!("eframe exited: {err:?}");
    }
}
