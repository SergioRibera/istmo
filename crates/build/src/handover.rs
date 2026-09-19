use bincode::config::Configuration;
use bincode::error::{DecodeError, EncodeError};

use crate::contract::Contract;
use crate::manifest::Manifest;
use crate::native_deps::NativeDeps;

const CODEC: Configuration = bincode::config::standard();

pub const CONTRACT_KEY: &str = "CONTRACT";

pub const NATIVE_DEPS_KEY: &str = "NATIVE_DEPS";

pub const MANIFEST_KEY: &str = "ISTMO_MANIFEST";

#[derive(Debug)]
pub enum HandoverError {

    InvalidHex,

    Decode(DecodeError),

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

pub fn serialize_contract(contract: &Contract) -> Result<String, HandoverError> {
    let bytes = bincode::encode_to_vec(contract, CODEC).map_err(HandoverError::Encode)?;
    Ok(hex_encode(&bytes))
}

pub fn deserialize_contract(hex: &str) -> Result<Contract, HandoverError> {
    let bytes = hex_decode(hex)?;
    let (contract, _) =
        bincode::decode_from_slice::<Contract, _>(&bytes, CODEC).map_err(HandoverError::Decode)?;
    Ok(contract)
}

pub fn serialize_native_deps(deps: &NativeDeps) -> Result<String, HandoverError> {
    let bytes = bincode::encode_to_vec(deps, CODEC).map_err(HandoverError::Encode)?;
    Ok(hex_encode(&bytes))
}

pub fn deserialize_native_deps(hex: &str) -> Result<NativeDeps, HandoverError> {
    let bytes = hex_decode(hex)?;
    let (deps, _) = bincode::decode_from_slice::<NativeDeps, _>(&bytes, CODEC)
        .map_err(HandoverError::Decode)?;
    Ok(deps)
}

pub fn emit_contract(contract: &Contract) {
    let payload = serialize_contract(contract).expect("serialize istmo contract");
    println!("cargo:{CONTRACT_KEY}={payload}");
}

pub fn emit_native_deps(deps: &NativeDeps) {
    let payload = serialize_native_deps(deps).expect("serialize istmo native deps");
    println!("cargo:{NATIVE_DEPS_KEY}={payload}");
}

pub fn serialize_manifest(manifest: &Manifest) -> Result<String, HandoverError> {
    let bytes = bincode::encode_to_vec(manifest, CODEC).map_err(HandoverError::Encode)?;
    Ok(hex_encode(&bytes))
}

pub fn deserialize_manifest(hex: &str) -> Result<Manifest, HandoverError> {
    let bytes = hex_decode(hex)?;
    let (manifest, _) = bincode::decode_from_slice::<Manifest, _>(&bytes, CODEC)
        .map_err(HandoverError::Decode)?;
    Ok(manifest)
}

pub fn emit_manifest(manifest: &Manifest) {
    let payload = serialize_manifest(manifest).expect("serialize istmo manifest");
    println!("cargo:{MANIFEST_KEY}={payload}");
}

#[must_use]
pub fn collect_dep_manifests() -> Vec<Manifest> {
    collect_env_payloads(MANIFEST_KEY)
        .into_iter()
        .filter_map(|(var, payload)| match deserialize_manifest(&payload) {
            Ok(m) => Some(m),
            Err(err) => {
                println!("cargo::warning=istmo handover decode failed for {var}: {err}");
                None
            }
        })
        .collect()
}

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

#[must_use]
pub fn collect_dep_native_deps() -> NativeDeps {
    let mut out = NativeDeps::new();
    for (var, payload) in collect_env_payloads(NATIVE_DEPS_KEY) {
        match deserialize_native_deps(&payload) {
            Ok(deps) => {
                out.merge(deps);
            }
            Err(err) => {
                println!("cargo::warning=istmo handover decode failed for {var}: {err}");
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
        let hex = "DEADBEEF";
        let err = deserialize_contract(hex).expect_err("bincode decode should fail");
        assert!(matches!(err, HandoverError::Decode(_)));
    }

    #[test]
    fn hex_decode_accepts_both_cases() {
        let upper = "48454C4C4F";
        let lower = "48454c4c4f";
        assert_eq!(hex_decode(upper).unwrap(), b"HELLO");
        assert_eq!(hex_decode(lower).unwrap(), b"HELLO");
    }
}

