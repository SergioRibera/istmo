//! Cross-crate metadata handover between plugin `build.rs` scripts and the
//! downstream app's `build.rs`.
//!
//! # Why
//!
//! `#[istmo::plugin]` proc-macros can describe a plugin's [`Contract`], but
//! Cargo runs each dependency's `build.rs` **before** the dependent crate's
//! proc-macros expand. Cross-crate metadata therefore has to travel through
//! Cargo's build-script channel: the plugin crate emits key/value pairs via
//! `cargo::metadata::…=…`; Cargo forwards them to every dependent build
//! script as `DEP_<links>_<KEY>` environment variables.
//!
//! This module wraps that pattern for the two payload types the workspace
//! actually crosses today:
//!
//! * [`Contract`] — one per plugin trait; downstream generates Kotlin /
//!   Swift / Rust glue against it.
//! * [`NativeDeps`] — Gradle coordinates + SPM products the plugin needs
//!   the app to add to its native build.
//!
//! Encoding is bincode → uppercase hex. Hex survives env-var round-trips
//! and stays under Cargo's practical limit for a metadata value (~a few KB
//! per emission fits every real contract we have).
//!
//! # Plugin-side usage (`build.rs`)
//!
//! ```no_run
//! let contract = istmo_build::Contract {
//!     plugin_id: "com.example.echo".to_owned(),
//!     type_name: "Echo".to_owned(),
//!     methods: vec![],
//!     init: None,
//!     types: vec![],
//! };
//! istmo_build::emit_contract(&contract);
//! ```
//!
//! Add `links = "istmo_com_example_echo"` (or any unique string) to the
//! plugin crate's `[package]` in `Cargo.toml` — without it Cargo does not
//! propagate the metadata pair to downstream build scripts.
//!
//! # Downstream usage (`build.rs`)
//!
//! ```no_run
//! for contract in istmo_build::collect_dep_contracts() {
//!     let out_dir = std::env::var("OUT_DIR").unwrap();
//!     let kt = istmo_build::generate_kotlin_client(&contract);
//!     let path = format!("{out_dir}/{}.kt", contract.type_name);
//!     std::fs::write(path, kt).unwrap();
//! }
//! ```

use bincode::config::Configuration;
use bincode::error::{DecodeError, EncodeError};

use crate::contract::Contract;
use crate::native_deps::NativeDeps;

const CODEC: Configuration = bincode::config::standard();

/// Metadata key used for the [`Contract`] emission. Consumers see it as
/// `DEP_<links>_CONTRACT` in their build-script environment.
pub const CONTRACT_KEY: &str = "CONTRACT";
/// Metadata key used for the [`NativeDeps`] emission. Consumers see it as
/// `DEP_<links>_NATIVE_DEPS`.
pub const NATIVE_DEPS_KEY: &str = "NATIVE_DEPS";

/// Failure modes when decoding a hex-encoded bincode payload back into a
/// typed value.
#[derive(Debug)]
pub enum HandoverError {
    /// The env-var payload was not valid hex.
    InvalidHex,
    /// The hex decoded but bincode could not parse the bytes.
    Decode(DecodeError),
    /// Encoding path failed. Only reachable from the emit helpers.
    Encode(EncodeError),
}

impl std::fmt::Display for HandoverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidHex => f.write_str("handover payload was not valid hex"),
            Self::Decode(err) => write!(f, "handover decode: {err}"),
            Self::Encode(err) => write!(f, "handover encode: {err}"),
        }
    }
}

impl std::error::Error for HandoverError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(err) => Some(err),
            Self::Encode(err) => Some(err),
            Self::InvalidHex => None,
        }
    }
}

/// bincode-encode + hex-encode a [`Contract`] for transit through a
/// `cargo::metadata::` emission.
pub fn serialize_contract(contract: &Contract) -> Result<String, HandoverError> {
    let bytes = bincode::encode_to_vec(contract, CODEC).map_err(HandoverError::Encode)?;
    Ok(hex_encode(&bytes))
}

/// Reverse of [`serialize_contract`].
pub fn deserialize_contract(hex: &str) -> Result<Contract, HandoverError> {
    let bytes = hex_decode(hex)?;
    let (contract, _) =
        bincode::decode_from_slice::<Contract, _>(&bytes, CODEC).map_err(HandoverError::Decode)?;
    Ok(contract)
}

/// bincode-encode + hex-encode a [`NativeDeps`] bundle for transit through
/// a `cargo::metadata::` emission.
pub fn serialize_native_deps(deps: &NativeDeps) -> Result<String, HandoverError> {
    let bytes = bincode::encode_to_vec(deps, CODEC).map_err(HandoverError::Encode)?;
    Ok(hex_encode(&bytes))
}

/// Reverse of [`serialize_native_deps`].
pub fn deserialize_native_deps(hex: &str) -> Result<NativeDeps, HandoverError> {
    let bytes = hex_decode(hex)?;
    let (deps, _) =
        bincode::decode_from_slice::<NativeDeps, _>(&bytes, CODEC).map_err(HandoverError::Decode)?;
    Ok(deps)
}

/// Emits the `cargo::metadata::CONTRACT=…` pair for `contract`. Call once
/// per plugin trait from the plugin crate's `build.rs`. Prints to stdout,
/// as Cargo expects.
///
/// The plugin crate must carry a `links = "…"` entry in `Cargo.toml`; without
/// it, Cargo silently drops metadata emissions for consumer scripts.
///
/// # Panics
/// Panics if serialization fails. `Contract` fields are plain owned types
/// so encoding failure indicates a bincode bug rather than a caller
/// mistake — surfacing it as a build-time panic keeps the API terse.
pub fn emit_contract(contract: &Contract) {
    let payload = serialize_contract(contract).expect("serialize istmo contract");
    println!("cargo::metadata::{CONTRACT_KEY}={payload}");
}

/// Emits the `cargo::metadata::NATIVE_DEPS=…` pair for `deps`.
///
/// # Panics
/// See [`emit_contract`] — same reasoning.
pub fn emit_native_deps(deps: &NativeDeps) {
    let payload = serialize_native_deps(deps).expect("serialize istmo native deps");
    println!("cargo::metadata::{NATIVE_DEPS_KEY}={payload}");
}

/// Collects every [`Contract`] emitted by a dependency's build script.
///
/// Walks the calling `build.rs` environment for `DEP_*_CONTRACT`
/// variables and decodes each. Values that fail to decode are logged via
/// `cargo::warning` and skipped so a single bad dependency does not
/// brick the whole build.
#[must_use]
pub fn collect_dep_contracts() -> Vec<Contract> {
    collect_env_payloads(CONTRACT_KEY)
        .into_iter()
        .filter_map(|(var, payload)| match deserialize_contract(&payload) {
            Ok(c) => Some(c),
            Err(err) => {
                println!("cargo::warning=istmo handover decode failed for {var}: {err}");
                None
            }
        })
        .collect()
}

/// Collects every dependency's [`NativeDeps`] emission into one merged
/// bundle.
///
/// Walks the calling `build.rs` environment for `DEP_*_NATIVE_DEPS`
/// variables and merges each via [`NativeDeps::merge`]. Version
/// conflicts accumulate on the returned bundle exactly as if the caller
/// had merged them by hand.
#[must_use]
pub fn collect_dep_native_deps() -> NativeDeps {
    let mut out = NativeDeps::new();
    for (var, payload) in collect_env_payloads(NATIVE_DEPS_KEY) {
        match deserialize_native_deps(&payload) {
            Ok(deps) => {
                out.merge(deps);
            }
            Err(err) => {
                println!(
                    "cargo::warning=istmo handover decode failed for {var}: {err}",
                );
            }
        }
    }
    out
}

fn collect_env_payloads(key: &str) -> Vec<(String, String)> {
    let suffix = format!("_{key}");
    std::env::vars()
        .filter(|(k, _)| k.starts_with("DEP_") && k.ends_with(&suffix))
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // upper-hex — env vars are case-preserving on every platform we
        // target, but the choice is arbitrary; decoding accepts either.
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{byte:02X}"));
    }
    out
}

fn hex_decode(hex: &str) -> Result<Vec<u8>, HandoverError> {
    if hex.len() % 2 != 0 {
        return Err(HandoverError::InvalidHex);
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks(2) {
        let hi = hex_digit(chunk[0])?;
        let lo = hex_digit(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

const fn hex_digit(byte: u8) -> Result<u8, HandoverError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(HandoverError::InvalidHex),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Arg, Method, MethodKind, TypeRef};
    use crate::native_deps::{GradleCoord, GradleDep, GradleScope};

    fn sample_contract() -> Contract {
        Contract {
            plugin_id: "com.example.roundtrip".to_owned(),
            type_name: "Echo".to_owned(),
            methods: vec![Method {
                name: "ping".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "msg".to_owned(),
                    ty: TypeRef::String,
                }],
                returns: TypeRef::String,
                error: None,
            }],
            init: None,
            types: vec![],
        }
    }

    #[test]
    fn contract_round_trip_matches_original() {
        let contract = sample_contract();
        let hex = serialize_contract(&contract).expect("serialize");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        let back = deserialize_contract(&hex).expect("deserialize");
        assert_eq!(back, contract);
    }

    #[test]
    fn native_deps_round_trip_matches_original() {
        let mut deps = NativeDeps::new();
        deps.add_gradle(&GradleDep::new(
            GradleScope::Implementation,
            GradleCoord::new("androidx.core", "core-ktx", "1.13.0"),
        ));
        let hex = serialize_native_deps(&deps).expect("serialize");
        let back = deserialize_native_deps(&hex).expect("deserialize");
        assert_eq!(back, deps);
    }

    #[test]
    fn deserialize_reports_invalid_hex() {
        let err = deserialize_contract("not-hex-at-all").expect_err("should fail");
        assert!(matches!(err, HandoverError::InvalidHex));
    }

    #[test]
    fn deserialize_reports_decode_failure_on_valid_hex_bad_bytes() {
        let hex = "DEADBEEF"; // valid hex, garbage as bincode.
        let err = deserialize_contract(hex).expect_err("bincode decode should fail");
        assert!(matches!(err, HandoverError::Decode(_)));
    }

    #[test]
    fn hex_decode_accepts_both_cases() {
        let upper = "48454C4C4F"; // "HELLO"
        let lower = "48454c4c4f";
        assert_eq!(hex_decode(upper).unwrap(), b"HELLO");
        assert_eq!(hex_decode(lower).unwrap(), b"HELLO");
    }
}
