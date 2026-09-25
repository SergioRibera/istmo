use istmo_build::{Contract, extract_contract};

const PLUGIN_SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs");

#[must_use]
pub fn contract() -> Contract {
    extract_contract(PLUGIN_SRC, "Biometric")
        .expect("extract Biometric contract from istmo-biometric/src/lib.rs")
}
