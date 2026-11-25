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
    not(all(
        target_os = "linux",
        any(feature = "linux-webkit2gtk-4-1", feature = "linux-qt")
    )),
    allow(
        dead_code,
        reason = "hooks only a real engine calls; the mock skips them"
    )
)]
mod core;
#[cfg_attr(
    not(all(
        target_os = "linux",
        any(feature = "linux-webkit2gtk-4-1", feature = "linux-qt")
    )),
    allow(dead_code, reason = "popups are opened by a real engine")
)]
mod host;
#[cfg(all(
    target_os = "linux",
    any(
        feature = "linux-webkit2gtk-4-1",
        feature = "linux-webkitgtk-6-0",
        feature = "linux-qt"
    )
))]
#[allow(
    unsafe_code,
    reason = "hand-written GLib / GTK / WebKit and Qt shim bindings"
)]
mod linux;
#[cfg(all(target_os = "macos", feature = "desktop"))]
#[allow(unsafe_code, reason = "AppKit / WebKit through objc2")]
mod macos;
#[cfg(test)]
mod mock;
pub(crate) mod text;
mod timer;
#[cfg(all(target_arch = "wasm32", feature = "web"))]
mod web;
#[cfg(all(target_arch = "wasm32", feature = "web"))]
pub(crate) use self::web::random_bytes as web_random_bytes;
#[cfg(all(target_os = "windows", feature = "desktop"))]
#[allow(unsafe_code, reason = "WebView2 COM through windows-rs")]
mod windows;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};

use istmo_core::Runtime;
use istmo_core::sync::lock;

use self::core::Engine;
pub use self::host::DesktopWebView;
use crate::{WebViewError, WebViewFactory, WebViewInitConfig};

/// Linux engines, in the order [`DesktopOptions::linux_backends`] tries
/// them. Only the ones compiled in (Cargo features) are candidates, and
/// only one of them can live in a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LinuxBackend {
    /// `libwebkit2gtk-4.1` (GTK 3, libsoup 3): windows on X11 and
    /// Wayland, embedding on X11.
    WebKit2Gtk41,
    /// `libwebkitgtk-6.0` (GTK 4): windows only.
    WebKitGtk60,
    /// Qt 6 `WebEngine` (Chromium) through `libistmo_webview_qt.so`:
    /// windows on X11 and Wayland, embedding on X11. Custom schemes are
    /// registered once, from the first [`WebViewInitConfig`].
    QtWebEngine,
}

/// Desktop-only knobs (the wire init config stays portable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopOptions {
    /// Linux engine preference.
    pub linux_backends: Vec<LinuxBackend>,
    /// Path of `libistmo_webview_qt.so`. `None` looks in
    /// `ISTMO_WEBVIEW_QT_SHIM`, next to the executable, in its `../lib`,
    /// at the copy this crate's build produced, then on the loader path.
    pub qt_shim: Option<PathBuf>,
}

impl Default for DesktopOptions {
    fn default() -> Self {
        Self {
            linux_backends: vec![
                LinuxBackend::WebKit2Gtk41,
                LinuxBackend::WebKitGtk60,
                LinuxBackend::QtWebEngine,
            ],
            qt_shim: None,
        }
    }
}

impl DesktopOptions {
    /// Linux engines to try, in order.
    #[must_use]
    pub fn with_linux_backends(mut self, backends: impl Into<Vec<LinuxBackend>>) -> Self {
        self.linux_backends = backends.into();
        self
    }

    /// Explicit location of the Qt shim library.
    #[must_use]
    pub fn with_qt_shim(mut self, path: impl Into<PathBuf>) -> Self {
        self.qt_shim = Some(path.into());
        self
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

    /// The shared engine; the first `config` seen starts it.
    fn engine(&self, config: &WebViewInitConfig) -> Result<Arc<dyn Engine>, WebViewError> {
        lock(&self.engine)
            .get_or_insert_with(|| start_engine(&self.options, config))
            .clone()
    }
}

#[cfg(all(
    target_os = "linux",
    any(
        feature = "linux-webkit2gtk-4-1",
        feature = "linux-webkitgtk-6-0",
        feature = "linux-qt"
    )
))]
fn start_engine(
    options: &DesktopOptions,
    config: &WebViewInitConfig,
) -> Result<Arc<dyn Engine>, WebViewError> {
    linux::start(options, config)
}

#[cfg(all(target_os = "macos", feature = "desktop"))]
#[expect(clippy::unnecessary_wraps, reason = "same signature on every platform")]
fn start_engine(
    _options: &DesktopOptions,
    _config: &WebViewInitConfig,
) -> Result<Arc<dyn Engine>, WebViewError> {
    Ok(macos::MacEngine::start())
}

#[cfg(all(target_os = "windows", feature = "desktop"))]
fn start_engine(
    _options: &DesktopOptions,
    _config: &WebViewInitConfig,
) -> Result<Arc<dyn Engine>, WebViewError> {
    Ok(windows::WinEngine::start()?)
}

#[cfg(all(target_arch = "wasm32", feature = "web"))]
fn start_engine(
    _options: &DesktopOptions,
    _config: &WebViewInitConfig,
) -> Result<Arc<dyn Engine>, WebViewError> {
    Ok(web::WebEngine::start()?)
}

#[cfg(not(any(
    all(
        target_os = "linux",
        any(
            feature = "linux-webkit2gtk-4-1",
            feature = "linux-webkitgtk-6-0",
            feature = "linux-qt"
        )
    ),
    all(any(target_os = "macos", target_os = "windows"), feature = "desktop"),
    all(target_arch = "wasm32", feature = "web"),
)))]
fn start_engine(
    _options: &DesktopOptions,
    _config: &WebViewInitConfig,
) -> Result<Arc<dyn Engine>, WebViewError> {
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
            .and_then(|_| self.engine(&config));
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
        all(
            target_os = "linux",
            any(
                feature = "linux-webkit2gtk-4-1",
                feature = "linux-webkitgtk-6-0",
                feature = "linux-qt"
            )
        ),
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

/// MIME type and bytes of the app asset a scheme URL `path` names under
/// `dir`, or the HTTP status that refuses it. The path is percent-decoded
/// once; empty, `.` / `..` segments, backslashes and NULs are refused.
#[cfg_attr(
    not(any(
        all(
            target_os = "linux",
            any(
                feature = "linux-webkit2gtk-4-1",
                feature = "linux-webkitgtk-6-0",
                feature = "linux-qt"
            )
        ),
        all(any(target_os = "macos", target_os = "windows"), feature = "desktop")
    )),
    allow(dead_code, reason = "used by the engines")
)]
pub(crate) fn read_asset(dir: &str, path: &str) -> Result<(&'static str, Vec<u8>), u16> {
    let rest = path.trim_start_matches('/');
    let rest = if rest.is_empty() { "index.html" } else { rest };
    // `+` stays literal: form decoding would turn it into a space.
    let rest = url::form_urlencoded::parse(format!("p={}", rest.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .ok_or(400_u16)?;
    if rest
        .split('/')
        .any(|s| s.is_empty() || s == "." || s == "..")
        || rest.contains(['\\', '\0'])
    {
        return Err(400);
    }
    let mime = mime_for(&rest).ok_or(415_u16)?;
    let dir = dir.trim_matches('/');
    let key = if dir.is_empty() {
        rest
    } else {
        format!("{dir}/{rest}")
    };
    istmo_core::assets::read(&key)
        .map(|bytes| (mime, bytes))
        .map_err(|_| 404)
}

#[cfg(test)]
mod asset_tests {
    use super::read_asset;

    #[test]
    fn refuses_traversal_and_unknown_types() {
        assert_eq!(read_asset("web", "/../secret.html"), Err(400));
        assert_eq!(read_asset("web", "/a/%2e%2e/b.html"), Err(400));
        assert_eq!(read_asset("web", "/a//b.html"), Err(400));
        assert_eq!(read_asset("web", "/a%5Cb.html"), Err(400));
        assert_eq!(read_asset("web", "/a%00.html"), Err(400));
        assert_eq!(read_asset("web", "/run.exe"), Err(415));
        assert_eq!(read_asset("web", "/missing+file.html"), Err(404));
    }
}
