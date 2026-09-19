use istmo_build::{Contract, extract_contract};

const PLUGIN_SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs");

#[must_use]
pub fn contract() -> Contract {
    extract_contract(PLUGIN_SRC, "SignIn")
        .expect("extract SignIn contract from istmo-google-sign-in/src/lib.rs")
}

