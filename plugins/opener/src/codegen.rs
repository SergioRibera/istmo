//! Build-time access to the opener [`Contract`], for apps that drive
//! `istmo-build` codegen themselves instead of reading the
//! `DEP_ISTMO_OPENER_ISTMO_CONTRACT` handover.

use istmo_build::{Contract, extract_contract};

const PLUGIN_SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs");

/// The `Opener` contract, extracted from this crate's own source.
///
/// # Panics
///
/// If `src/lib.rs` of this crate no longer parses into an `Opener`
/// contract — a bug in the published crate, never a runtime condition.
#[must_use]
pub fn contract() -> Contract {
    extract_contract(PLUGIN_SRC, "Opener")
        .expect("extract Opener contract from istmo-opener/src/lib.rs")
}
