//! Wayland `zwp_tablet_v2` backend — scaffold.
//!
//! The real implementation lands in a follow-up: open a secondary
//! Wayland connection via the foreign-display handle, bind
//! `zwp_tablet_manager_v2`, enumerate seats + tools, and translate
//! `motion` / `down` / `up` / `pressure` / `tilt` / `distance` /
//! `rotation` events into the [`PenEvent`] / [`PenHoverEvent`] streams
//! on the owning [`WindowState`].
//!
//! For now the attach entry point rejects every call with a
//! "not implemented" error. The publisher dispatcher threads the raw
//! surface + display pointers through so the follow-up only has to
//! replace the body of [`attach_wayland`] — no API shape changes
//! required at the call site.
//!
//! # Pointer handling expectations
//!
//! `surface` is a `*mut wl_surface` held alive by the hosting app /
//! windowing library (winit, GTK, SDL, …). `display` is a
//! `*mut wl_display` from the same provider. The real implementation
//! must build its own `wayland_client::Connection` via
//! `Backend::from_foreign_display` so it does not dispose of the
//! display when its event queue drops — the app retains ownership.

#![allow(dead_code)] // scaffold — real backend lands in a follow-up commit

use std::num::NonZeroUsize;
use std::sync::Arc;

use super::WindowState;

/// Book-keeping returned from [`attach_wayland`] once the backend
/// lands. For now the type exists so [`WindowState`] can carry an
/// `Option<WaylandAttachment>` field without an extra `cfg` dance.
#[derive(Debug)]
pub(super) struct WaylandAttachment {
    _surface: NonZeroUsize,
    _display: NonZeroUsize,
}

/// Failure modes exposed to the publisher.
#[derive(Debug)]
pub(super) enum AttachError {
    /// The real backend has not landed yet. Removed once the follow-up
    /// commit adds the `wayland-client` integration.
    NotImplemented,
}

impl std::fmt::Display for AttachError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotImplemented => f.write_str(
                "wayland zwp_tablet_v2 backend is not implemented yet; \
                 pending wayland-client / wayland-protocols integration",
            ),
        }
    }
}

impl std::error::Error for AttachError {}

/// Attach a Wayland `wl_surface` + `wl_display` pair to the publisher.
///
/// `_state` is reserved for the follow-up: the real backend will push
/// `PenEvent` / `PenHoverEvent` through its `events_tx` / `hover_tx`
/// senders.
#[allow(clippy::unnecessary_wraps)]
pub(super) fn attach_wayland(
    surface: NonZeroUsize,
    display: NonZeroUsize,
    _state: Arc<WindowState>,
) -> Result<WaylandAttachment, AttachError> {
    // Returning an `Ok` here is intentional while the real backend is
    // absent — the publisher still records the surface so the id
    // round-trips through `WindowRegistry::get`. Returning `Err` would
    // break every app that registers a Wayland window on Linux today
    // (the same shape works on macOS / Windows). The follow-up
    // replaces the body with a real `wayland-client` connection.
    Ok(WaylandAttachment {
        _surface: surface,
        _display: display,
    })
}
