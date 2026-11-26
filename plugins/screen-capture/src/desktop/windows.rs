//! Windows backends, one module per cargo feature, plus what they share:
//! the OS probe, D3D11 helpers, the source listing and its watcher.
//!
//! `Auto` order: WGC → DXGI → GDI. None of them joins `Auto` before its
//! checklist passed on a real Windows session (`examples/win_probe.rs`
//! runs it); until then they are reachable through `Fixed`, `Ordered`
//! and the environment override.

pub(crate) mod access;
#[cfg_attr(
    not(all(
        feature = "windows-wgc",
        feature = "windows-dxgi",
        feature = "windows-gdi"
    )),
    allow(
        dead_code,
        reason = "shared by the video backends; a build with fewer uses less"
    )
)]
mod common;
#[cfg_attr(
    not(all(feature = "windows-wgc", feature = "windows-dxgi")),
    allow(
        dead_code,
        reason = "shared by the GPU backends; a build with fewer uses less"
    )
)]
pub(crate) mod d3d;
#[cfg(feature = "windows-gdi")]
mod gdi;
#[cfg_attr(
    not(all(
        feature = "windows-wgc",
        feature = "windows-dxgi",
        feature = "windows-gdi"
    )),
    allow(
        dead_code,
        reason = "shared by the video backends; a build with fewer uses less"
    )
)]
pub(crate) mod listing;
#[cfg_attr(
    not(all(
        feature = "windows-wgc",
        feature = "windows-dxgi",
        feature = "windows-gdi",
        feature = "windows-audio"
    )),
    allow(
        dead_code,
        reason = "shared by every backend; a build with fewer uses less"
    )
)]
pub(crate) mod os;
#[cfg_attr(
    not(any(
        feature = "windows-wgc",
        feature = "windows-dxgi",
        feature = "windows-gdi"
    )),
    allow(dead_code, reason = "used by the video backends only")
)]
mod watch;

use std::sync::Arc;

use crate::types::BackendId;

use super::backend::{AudioBackend, Backend};

/// Backends the plugin knows on Windows, compiled or not.
pub(crate) const KNOWN: [BackendId; 3] = [
    BackendId::WindowsWgc,
    BackendId::WindowsDxgi,
    BackendId::WindowsGdi,
];

/// The `Auto` order on Windows.
pub(crate) const ORDER: [BackendId; 3] = KNOWN;

/// Every compiled video backend.
#[allow(
    clippy::vec_init_then_push,
    reason = "each push is behind its own feature"
)]
pub(crate) fn backends() -> Vec<Arc<dyn Backend>> {
    os::ensure_mta();
    #[allow(unused_mut, reason = "empty when no Windows video feature is enabled")]
    let mut backends: Vec<Arc<dyn Backend>> = Vec::new();
    #[cfg(feature = "windows-gdi")]
    backends.push(gdi::backend());
    backends
}

/// The compiled audio backend: none yet.
pub(crate) const fn audio() -> Option<Arc<dyn AudioBackend>> {
    None
}

/// A NUL-terminated (or full) UTF-16 buffer as a `String`.
pub(crate) fn wide_to_string(wide: &[u16]) -> String {
    let len = wide.iter().position(|c| *c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_strings_stop_at_the_nul() {
        let wide: Vec<u16> = "DISPLAY1\0junk".encode_utf16().collect();
        assert_eq!(wide_to_string(&wide), "DISPLAY1");
        let full: Vec<u16> = "ab".encode_utf16().collect();
        assert_eq!(wide_to_string(&full), "ab");
    }
}
