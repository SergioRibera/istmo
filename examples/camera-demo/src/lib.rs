//! Shared library crate so `main.rs` (native) and the wasm entry
//! point can both pull in [`CameraApp`] without duplicating code.

#![doc(html_root_url = "https://docs.rs/camera-demo")]

mod app;

pub use app::CameraApp;

use std::sync::Arc;

use istmo_camera::{CameraClient, CameraInitConfig};
use istmo_core::{Runtime, RuntimeConfig, RuntimeInit};

/// Build the runtime, register the correct host for the target, and
/// return a ready-to-call [`CameraClient`] wrapped in `Arc` so it can
/// be cloned into every open session.
///
/// # Errors
/// Returns the runtime bring-up error. The platform-specific host
/// registration is infallible.
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
fn register_host(runtime: &Arc<Runtime>) {
    use istmo_camera::{CameraHost, DesktopCameraFactory};
    runtime.register_host(CameraHost::new(DesktopCameraFactory));
}

#[cfg(target_arch = "wasm32")]
fn register_host(runtime: &Arc<Runtime>) {
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
