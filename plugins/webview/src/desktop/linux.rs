//! Linux engines.
//!
//! Only one GTK / `WebKit` major version can live in a process (`GType`
//! names and libsoup versions clash), and Qt owns its own application
//! object, so the first backend that starts is the only one.

#[cfg(any(feature = "linux-webkit2gtk-4-1", feature = "linux-webkitgtk-6-0"))]
mod ffi;
#[cfg(any(feature = "linux-webkit2gtk-4-1", feature = "linux-webkitgtk-6-0"))]
mod gtk;
#[cfg(feature = "linux-qt")]
mod qt;
#[cfg(any(feature = "linux-webkit2gtk-4-1", feature = "linux-webkitgtk-6-0"))]
mod x11;

use std::sync::Arc;

use super::core::Engine;
use super::{DesktopOptions, LinuxBackend};
use crate::{WebViewError, WebViewInitConfig};

#[cfg(any(feature = "linux-webkit2gtk-4-1", feature = "linux-qt"))]
/// Whether the toolkit must use X11: the app registered X11 windows
/// (embedding targets), or there is no Wayland session at all.
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

/// Storage directory of persistent profile `name`, after checking the
/// name is a plain file name (`[A-Za-z0-9._-]`, 1–64 chars).
fn profile_dir(name: &str) -> Result<String, WebViewError> {
    if name.is_empty()
        || name.len() > 64
        || name.starts_with('.')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(WebViewError::InvalidArgument("profile name".into()));
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
        })
        .unwrap_or_else(std::env::temp_dir);
    let app = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "istmo-app".into());
    Ok(base
        .join(app)
        .join("istmo-webview")
        .join(name)
        .to_string_lossy()
        .into_owned())
}

#[cfg_attr(
    not(feature = "linux-qt"),
    expect(unused_variables, reason = "only Qt registers schemes at start")
)]
pub(super) fn start(
    options: &DesktopOptions,
    config: &WebViewInitConfig,
) -> Result<Arc<dyn Engine>, WebViewError> {
    let mut last = WebViewError::Unsupported("no Linux web engine compiled in".into());
    for backend in &options.linux_backends {
        let started: Result<Arc<dyn Engine>, WebViewError> = match backend {
            #[cfg(feature = "linux-webkit2gtk-4-1")]
            LinuxBackend::WebKit2Gtk41 => {
                gtk::GtkEngine::start(gtk::Flavor::Gtk3, wants_x11()).map(|e| e as Arc<dyn Engine>)
            }
            #[cfg(feature = "linux-webkitgtk-6-0")]
            LinuxBackend::WebKitGtk60 => {
                gtk::GtkEngine::start(gtk::Flavor::Gtk4, false).map(|e| e as Arc<dyn Engine>)
            }
            #[cfg(feature = "linux-qt")]
            LinuxBackend::QtWebEngine => qt::QtEngine::start(
                options.qt_shim.as_ref(),
                config.schemes.iter().map(|s| s.name.clone()).collect(),
                wants_x11(),
            )
            .map(|e| e as Arc<dyn Engine>),
            #[allow(
                unreachable_patterns,
                reason = "unreachable when every Linux engine is compiled in"
            )]
            other => {
                tracing::debug!(?other, "web engine not compiled in");
                continue;
            }
        };
        match started {
            Ok(engine) => return Ok(engine),
            Err(err) => {
                tracing::debug!(?backend, %err, "web engine unavailable");
                last = err;
            }
        }
    }
    Err(last)
}
