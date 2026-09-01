//! Facade crate for the istmo framework.
//!
//! Application and plugin authors depend on this crate; it re-exports the
//! runtime API from [`istmo_core`], the proc-macros from `istmo_macros` and
//! a curated slice of [`bincode`] under the `bincode` sub-path so that
//! generated glue can name `::istmo::bincode::Encode` without forcing users
//! to add `bincode` to their own `Cargo.toml`.

pub use istmo_core::*;
pub use istmo_macros::{message, plugin, stream};

/// Re-exported [`bincode`] surface used by generated code. Not part of the
/// stable public API — pinned exclusively for macro output.
pub mod bincode {
    pub use ::bincode::error;
    pub use ::bincode::{Decode, Encode, config, decode_from_slice, encode_to_vec};
}
