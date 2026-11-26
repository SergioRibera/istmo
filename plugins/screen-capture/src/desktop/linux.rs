//! Linux backends, one module per cargo feature, and session detection.

#[cfg(feature = "linux-ext-image-copy")]
mod ext_image_copy;
#[cfg(feature = "dmabuf-alloc")]
mod gbm;
#[cfg(any(feature = "linux-portal", feature = "wayland", feature = "linux-x11"))]
mod mapping;
#[cfg(feature = "pipewire")]
mod pipewire;
#[cfg(feature = "linux-audio")]
mod pipewire_audio;
#[cfg(feature = "linux-portal")]
mod portal;
#[cfg(feature = "linux-portal")]
mod syncobj;
#[cfg(feature = "wayland")]
mod wayland;
#[cfg(feature = "linux-wlr-screencopy")]
mod wlr_screencopy;
#[cfg(feature = "linux-x11")]
mod x11;

use std::sync::Arc;

use crate::select::SessionKind;
use crate::types::BackendId;

use super::backend::{AudioBackend, Backend};

/// Backends the plugin knows on Linux, compiled or not.
pub(crate) const KNOWN: [BackendId; 5] = [
    BackendId::LinuxExtImageCopy,
    BackendId::LinuxWlrScreencopy,
    BackendId::LinuxPortal,
    BackendId::LinuxX11,
    BackendId::LinuxKms,
];

/// Every compiled video backend.
#[allow(
    clippy::vec_init_then_push,
    reason = "each push is behind its own feature"
)]
pub(crate) fn backends() -> Vec<Arc<dyn Backend>> {
    #[allow(unused_mut, reason = "empty when no Linux backend feature is enabled")]
    let mut backends: Vec<Arc<dyn Backend>> = Vec::new();
    #[cfg(feature = "linux-ext-image-copy")]
    backends.push(ext_image_copy::backend());
    #[cfg(feature = "linux-wlr-screencopy")]
    backends.push(wlr_screencopy::backend());
    #[cfg(feature = "linux-portal")]
    backends.push(portal::backend());
    #[cfg(feature = "linux-x11")]
    backends.push(x11::backend());
    backends
}

/// The compiled audio backend.
#[allow(clippy::unnecessary_wraps, reason = "`None` when `linux-audio` is off")]
pub(crate) fn audio() -> Option<Arc<dyn AudioBackend>> {
    #[cfg(feature = "linux-audio")]
    {
        Some(pipewire_audio::backend())
    }
    #[cfg(not(feature = "linux-audio"))]
    {
        None
    }
}

fn env_is(name: &str, value: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v.eq_ignore_ascii_case(value))
}

/// Whether this process runs under XWayland only (a Wayland session
/// with `DISPLAY` set): X11 capture would see X clients only.
pub(crate) fn wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || env_is("XDG_SESSION_TYPE", "wayland")
}

/// The kind of session this process runs in.
pub(crate) fn session_kind() -> SessionKind {
    let sandboxed = std::env::var_os("FLATPAK_ID").is_some()
        || std::path::Path::new("/.flatpak-info").exists()
        || std::env::var_os("SNAP").is_some();
    if sandboxed {
        SessionKind::Sandboxed
    } else if wayland_session() {
        SessionKind::Wayland
    } else if std::env::var_os("DISPLAY").is_some() {
        SessionKind::X11
    } else {
        SessionKind::Unknown
    }
}
