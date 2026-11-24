//! Linux engines.
//!
//! Only one GTK / `WebKit` major version can live in a process (`GType`
//! names and libsoup versions clash), so the first backend that starts
//! is the only one.

mod ffi;
#[cfg(feature = "linux-webkit2gtk-4-1")]
mod gtk;
mod x11;

use std::sync::Arc;

use super::LinuxBackend;
use super::core::Engine;
use crate::WebViewError;

/// Whether GDK must use X11: the app registered X11 windows (embedding
/// targets), or there is no Wayland session at all.
fn wants_x11() -> bool {
    let registry = istmo_window::WindowRegistry::global();
    let has_x11_window = registry.ids().into_iter().any(|id| {
        matches!(
            registry.get(id),
            Some(istmo_window::NativeWindow::Xlib { .. } | istmo_window::NativeWindow::Xcb { .. })
        )
    });
    has_x11_window || std::env::var_os("WAYLAND_DISPLAY").is_none()
}

pub(super) fn start(backends: &[LinuxBackend]) -> Result<Arc<dyn Engine>, WebViewError> {
    let mut last = WebViewError::Unsupported("no Linux web engine compiled in".into());
    for backend in backends {
        match backend {
            #[cfg(feature = "linux-webkit2gtk-4-1")]
            LinuxBackend::WebKit2Gtk41 => match gtk::GtkEngine::start(wants_x11()) {
                Ok(engine) => return Ok(engine),
                Err(err) => {
                    tracing::debug!(%err, "WebKitGTK 4.1 unavailable");
                    last = err;
                }
            },
            other => {
                tracing::debug!(?other, "web engine not compiled in");
            }
        }
    }
    Err(last)
}
