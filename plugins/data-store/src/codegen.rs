//! Downstream-facing `Contract` accessor for the `istmo.data_store`
//! plugin.
//!
//! Enabled via the `codegen` feature. Downstream apps that need to codegen
//! Kotlin / Swift host classes from `build.rs` add
//! `istmo-data-store = { workspace = true, features = ["codegen"] }`
//! under `[build-dependencies]` and call [`contract`] directly. Consumers
//! that go through the cross-crate metadata channel
//! (`istmo_build::collect_dep_contracts`) do not need this feature.
//!
//! The contract is extracted from this crate's own `src/lib.rs` at
//! consumer-build time via [`istmo_build::extract_contract`], so the trait
//! declaration in `lib.rs` remains the single source of truth.

use istmo_build::{Contract, extract_contract};

/// Absolute path to this plugin crate's `src/lib.rs`. `env!` expands at
/// **this** crate's compile time, so the resulting string points at the
/// plugin source no matter which downstream `build.rs` calls into
/// [`contract`].
const PLUGIN_SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs");

/// Extracts the `DataStore` contract from `src/lib.rs`.
///
/// # Panics
/// Panics if the source file cannot be read or the trait annotation drifts
/// away from a shape the extractor understands — either is a build-time
/// authoring error and there is no meaningful recovery.
#[must_use]
pub fn contract() -> Contract {
    extract_contract(PLUGIN_SRC, "DataStore")
        .expect("extract DataStore contract from istmo-data-store/src/lib.rs")
}
