//! Rust hosts for Linux, macOS and Windows.
//!
//! ```ignore
//! use istmo_webview::WebViewHost;
//! use istmo_webview::desktop::{DesktopOptions, DesktopWebViewFactory};
//! runtime.register_host(WebViewHost::new(DesktopWebViewFactory::new(&runtime, DesktopOptions::default())));
//! ```
//!
//! The engine runs on its own UI thread (Linux) or the app's main
//! thread (macOS, Windows). Never block on a web view call from that
//! thread: answers come back through it.

#[cfg_attr(
    not(all(target_os = "linux", feature = "linux-webkit2gtk-4-1")),
    allow(
        dead_code,
        reason = "hooks only a real engine calls; the mock skips them"
    )
)]
mod core;
#[cfg_attr(
    not(all(target_os = "linux", feature = "linux-webkit2gtk-4-1")),
    allow(dead_code, reason = "popups are opened by a real engine")
)]
mod host;
#[cfg(all(
    target_os = "linux",
    any(feature = "linux-webkit2gtk-4-1", feature = "linux-webkitgtk-6-0")
))]
#[allow(unsafe_code, reason = "hand-written GLib / GTK / WebKit bindings")]
mod linux;
#[cfg(all(target_os = "macos", feature = "desktop"))]
#[allow(unsafe_code, reason = "AppKit / WebKit through objc2")]
mod macos;
#[cfg(test)]
mod mock;
pub(crate) mod text;
#[cfg(all(target_os = "windows", feature = "desktop"))]
#[allow(unsafe_code, reason = "WebView2 COM through windows-rs")]
mod windows;

use std::sync::{Arc, Mutex, Weak};

use istmo_core::Runtime;
use istmo_core::sync::lock;

use self::core::Engine;
pub use self::host::DesktopWebView;
use crate::{WebViewError, WebViewFactory, WebViewInitConfig};

/// Linux engines, in the order [`DesktopOptions::linux_backends`] tries
/// them. Only the ones compiled in (Cargo features) are candidates, and
/// only one GTK / `WebKit` major version can live in a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LinuxBackend {
    /// `libwebkit2gtk-4.1` (GTK 3, libsoup 3): windows on X11 and
    /// Wayland, embedding on X11.
    WebKit2Gtk41,
    /// `libwebkitgtk-6.0` (GTK 4): windows only.
    WebKitGtk60,
}

/// Desktop-only knobs (the wire init config stays portable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopOptions {
    /// Linux engine preference.
    pub linux_backends: Vec<LinuxBackend>,
}

impl Default for DesktopOptions {
    fn default() -> Self {
        Self {
            linux_backends: vec![LinuxBackend::WebKit2Gtk41, LinuxBackend::WebKitGtk60],
        }
    }
}

/// Builds [`DesktopWebView`]s; every client instance shares one engine,
/// started on first use.
pub struct DesktopWebViewFactory {
    runtime: Weak<Runtime>,
    options: DesktopOptions,
    engine: Mutex<Option<Result<Arc<dyn Engine>, WebViewError>>>,
}

impl std::fmt::Debug for DesktopWebViewFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopWebViewFactory")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl DesktopWebViewFactory {
    /// Factory serving `runtime`.
    #[must_use]
    pub fn new(runtime: &Arc<Runtime>, options: DesktopOptions) -> Self {
        Self {
            runtime: Arc::downgrade(runtime),
            options,
            engine: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_engine(runtime: &Arc<Runtime>, engine: Arc<dyn Engine>) -> Self {
        Self {
            runtime: Arc::downgrade(runtime),
            options: DesktopOptions::default(),
            engine: Mutex::new(Some(Ok(engine))),
        }
    }

    fn engine(&self) -> Result<Arc<dyn Engine>, WebViewError> {
        lock(&self.engine)
            .get_or_insert_with(|| start_engine(&self.options))
            .clone()
    }
}

#[cfg(all(
    target_os = "linux",
    any(feature = "linux-webkit2gtk-4-1", feature = "linux-webkitgtk-6-0")
))]
fn start_engine(options: &DesktopOptions) -> Result<Arc<dyn Engine>, WebViewError> {
    linux::start(&options.linux_backends)
}

#[cfg(all(target_os = "macos", feature = "desktop"))]
#[expect(clippy::unnecessary_wraps, reason = "same signature on every platform")]
fn start_engine(_options: &DesktopOptions) -> Result<Arc<dyn Engine>, WebViewError> {
    Ok(macos::MacEngine::start())
}

#[cfg(all(target_os = "windows", feature = "desktop"))]
fn start_engine(_options: &DesktopOptions) -> Result<Arc<dyn Engine>, WebViewError> {
    Ok(windows::WinEngine::start()?)
}

#[cfg(not(any(
    all(
        target_os = "linux",
        any(feature = "linux-webkit2gtk-4-1", feature = "linux-webkitgtk-6-0")
    ),
    all(any(target_os = "macos", target_os = "windows"), feature = "desktop"),
)))]
fn start_engine(_options: &DesktopOptions) -> Result<Arc<dyn Engine>, WebViewError> {
    Err(WebViewError::Unsupported(
        "no desktop web engine compiled in for this target".into(),
    ))
}

impl WebViewFactory for DesktopWebViewFactory {
    type Instance = DesktopWebView;

    fn create(&self, config: WebViewInitConfig) -> DesktopWebView {
        let runtime = self.runtime.upgrade();
        let engine = runtime
            .as_ref()
            .ok_or_else(|| WebViewError::Backend("istmo runtime is gone".into()))
            .and_then(|_| self.engine());
        match runtime {
            Some(rt) => DesktopWebView::new(&rt, config, engine),
            None => DesktopWebView::detached(config),
        }
    }
}

/// MIME type served for an asset path (by extension); `None` refuses
/// the file rather than letting the engine sniff it.
#[cfg_attr(
    not(any(
        all(target_os = "linux", feature = "linux-webkit2gtk-4-1"),
        all(any(target_os = "macos", target_os = "windows"), feature = "desktop")
    )),
    allow(dead_code, reason = "used by the engines")
)]
pub(crate) fn mime_for(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase())?;
    Some(match ext.as_str() {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" => "text/plain",
        "xml" => "application/xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "pdf" => "application/pdf",
        _ => return None,
    })
}
