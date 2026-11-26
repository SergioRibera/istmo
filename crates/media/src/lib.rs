//! Message types that mean the same thing in several istmo media
//! plugins (`istmo-camera`, `istmo-screen-capture`).
//!
//! A plugin re-exports what it uses (`pub use istmo_media::AccessStatus`)
//! and its contract references the type; `istmo-build` generates each
//! type once per app (Kotlin package `dev.istmo.media`, Swift file
//! `Plugins/IstmoMedia/Generated/IstmoMediaTypes.swift`) however many
//! plugins use it, while every plugin's codecs read and write it.
//!
//! A type lives here only when its meaning is identical in every plugin
//! that uses it. Look-alikes that differ (camera's `PixelFormat` carries
//! `Jpeg`, screen capture's 10-bit and HDR layouts) stay in their plugin.

#![doc(html_root_url = "https://docs.rs/istmo-media")]

use istmo_macros::message;

/// The state of a standing permission (camera access, screen recording,
/// notifications). Each plugin documents which states its platforms
/// report; the first four are every permission's.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AccessStatus {
    Granted,
    /// Refused; asking again shows no prompt.
    Denied,
    /// Not asked yet: requesting access prompts.
    NotDetermined,
    /// Parental controls, MDM or enterprise policy forbid it; a prompt
    /// would not help.
    Restricted,
    /// Granted, but only a relaunched process gets it (macOS Screen
    /// Recording).
    RequiresRestart,
    /// No standing permission exists; consent is asked per session.
    PerSession,
    /// The platform has no such permission.
    NotApplicable,
}
