//! Facade crate for the istmo framework.
//!
//! Application and plugin authors depend on this crate; it re-exports the
//! runtime API from [`istmo_core`], the proc-macros from `istmo_macros` and
//! a curated slice of [`bincode`] under the `bincode` sub-path so that
//! generated glue can name `::istmo::bincode::Encode` without forcing users
//! to add `bincode` to their own `Cargo.toml`.

pub use istmo_core::*;
pub use istmo_macros::{message, mobile_app, plugin, runtime, service, stream, worker};

/// Core plugins bundled with the framework (permissions, app lifecycle,
/// deep links, activity results). See [`istmo_plugins`] for the full surface.
pub use istmo_plugins as plugins;

/// Re-export of the android JNI transport. The `istmo::runtime!` macro emits
/// `pub use ::istmo::android::entrypoint::*;` on android targets so users
/// never touch JNI symbols directly.
#[cfg(target_os = "android")]
pub use istmo_android as android;

/// Re-export of the Apple `@_cdecl` transport. The `istmo::runtime!` macro
/// emits `pub use ::istmo::ios::entrypoint::*;` on iOS / tvOS / watchOS /
/// visionOS targets so users never touch the FFI symbols directly. macOS
/// deliberately stays desktop-dev-only — no transport crate is linked
/// there.
#[cfg(any(
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
))]
pub use istmo_ios as ios;

/// Re-exported [`bincode`] surface used by generated code. Not part of the
/// stable public API — pinned exclusively for macro output.
pub mod bincode {
    pub use ::bincode::error;
    pub use ::bincode::{Decode, Encode, config, decode_from_slice, encode_to_vec};
}
