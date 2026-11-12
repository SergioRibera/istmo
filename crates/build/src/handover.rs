use std::collections::BTreeMap;
use std::path::PathBuf;

use wincode::config::Configuration;
use wincode::int_encoding::{LittleEndian, VarInt};
use wincode::len::BincodeLen;
use wincode::{ReadError as DecodeError, WriteError as EncodeError};

use crate::assets::AssetsPayload;
use crate::contract::Contract;
use crate::manifest::Manifest;
use crate::native_deps::NativeDeps;

/// Build-time handover between our own build scripts. Payloads are a few
/// KiB; the limit only keeps a broken `DEP_*` value from claiming memory.
type Codec = Configuration<true, { 64 << 20 }, BincodeLen, LittleEndian, VarInt>;

const fn codec() -> Codec {
    Codec::new()
}

pub const CONTRACT_KEY: &str = "ISTMO_CONTRACT";

pub const NATIVE_DEPS_KEY: &str = "ISTMO_NATIVE_DEPS";

/// Keys emitted before the `ISTMO_` prefix. Still read so dependencies
/// built by an older `istmo-build` keep working; any crate may emit a
/// bare `CONTRACT`, so a payload under these that fails to decode is
/// skipped without a warning.
const LEGACY_CONTRACT_KEY: &str = "CONTRACT";

const LEGACY_NATIVE_DEPS_KEY: &str = "NATIVE_DEPS";

pub const MANIFEST_KEY: &str = "ISTMO_MANIFEST";

pub const ASSETS_KEY: &str = "ISTMO_ASSETS";

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
    let bytes = wincode::config::serialize(contract, codec()).map_err(HandoverError::Encode)?;
    Ok(hex_encode(&bytes))
}

pub fn deserialize_contract(hex: &str) -> Result<Contract, HandoverError> {
    let bytes = hex_decode(hex)?;
    let contract = wincode::config::deserialize_exact::<Contract, _>(&bytes, codec())
        .map_err(HandoverError::Decode)?;
    Ok(contract)
}

pub fn serialize_native_deps(deps: &NativeDeps) -> Result<String, HandoverError> {
    let bytes = wincode::config::serialize(deps, codec()).map_err(HandoverError::Encode)?;
    Ok(hex_encode(&bytes))
}

pub fn deserialize_native_deps(hex: &str) -> Result<NativeDeps, HandoverError> {
    let bytes = hex_decode(hex)?;
    let deps = wincode::config::deserialize_exact::<NativeDeps, _>(&bytes, codec())
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
    let bytes = wincode::config::serialize(manifest, codec()).map_err(HandoverError::Encode)?;
    Ok(hex_encode(&bytes))
}

pub fn deserialize_manifest(hex: &str) -> Result<Manifest, HandoverError> {
    let bytes = hex_decode(hex)?;
    let manifest = wincode::config::deserialize_exact::<Manifest, _>(&bytes, codec())
        .map_err(HandoverError::Decode)?;
    Ok(manifest)
}

pub fn emit_manifest(manifest: &Manifest) {
    let payload = serialize_manifest(manifest).expect("serialize istmo manifest");
    println!("cargo:{MANIFEST_KEY}={payload}");
}

pub fn serialize_assets(payload: &AssetsPayload) -> Result<String, HandoverError> {
    let bytes = wincode::config::serialize(payload, codec()).map_err(HandoverError::Encode)?;
    Ok(hex_encode(&bytes))
}

pub fn deserialize_assets(hex: &str) -> Result<AssetsPayload, HandoverError> {
    let bytes = hex_decode(hex)?;
    let payload = wincode::config::deserialize_exact::<AssetsPayload, _>(&bytes, codec())
        .map_err(HandoverError::Decode)?;
    Ok(payload)
}

pub fn emit_assets(payload: &AssetsPayload) {
    let encoded = serialize_assets(payload).expect("serialize istmo assets payload");
    println!("cargo:{ASSETS_KEY}={encoded}");
}

/// Every dep-advertised assets bundle as `(links, payload)` pairs
/// sorted by `links` for deterministic output.
#[must_use]
pub fn collect_dep_assets() -> Vec<(String, AssetsPayload)> {
    let mut out: Vec<(String, AssetsPayload)> = collect_env_payloads(ASSETS_KEY)
        .into_iter()
        .filter_map(|(var, payload)| match deserialize_assets(&payload) {
            Ok(p) => Some((links_name(&var, ASSETS_KEY), p)),
            Err(err) => {
                println!("cargo::warning=istmo assets handover decode failed for {var}: {err}");
                None
            }
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[must_use]
pub fn collect_dep_manifests() -> Vec<Manifest> {
    collect_dep_manifests_by_links()
        .into_iter()
        .map(|(_, manifest)| manifest)
        .collect()
}

/// Like [`collect_dep_manifests`], paired with the `links` name of the
/// plugin crate that advertised each manifest (the `<LINKS>` in
/// `DEP_<LINKS>_ISTMO_MANIFEST`), sorted by that name.
#[must_use]
pub fn collect_dep_manifests_by_links() -> Vec<(String, Manifest)> {
    let mut out: Vec<(String, Manifest)> = collect_env_payloads(MANIFEST_KEY)
        .into_iter()
        .filter_map(|(var, payload)| match deserialize_manifest(&payload) {
            Ok(m) => Some((links_name(&var, MANIFEST_KEY), m)),
            Err(err) => {
                println!("cargo::warning=istmo handover decode failed for {var}: {err}");
                None
            }
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Platform directory a plugin crate ships under `native/`, advertised
/// by [`emit_manifest_metadata`](crate::emit_manifest_metadata) as
/// `DEP_<LINKS>_ISTMO_NATIVE_<PLATFORM>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativePlatform {
    Android,
    Ios,
    Macos,
}

impl NativePlatform {
    const fn env_key(self) -> &'static str {
        match self {
            Self::Android => "ISTMO_NATIVE_ANDROID",
            Self::Ios => "ISTMO_NATIVE_IOS",
            Self::Macos => "ISTMO_NATIVE_MACOS",
        }
    }
}

/// Every dependency's `native/<platform>/` directory as `(links, dir)`
/// pairs sorted by `links` name so generated output is deterministic.
#[must_use]
pub fn collect_dep_native_dirs(platform: NativePlatform) -> Vec<(String, PathBuf)> {
    let key = platform.env_key();
    let mut out: Vec<(String, PathBuf)> = collect_env_payloads(key)
        .into_iter()
        .map(|(var, dir)| (links_name(&var, key), PathBuf::from(dir)))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn links_name(var: &str, key: &str) -> String {
    var.strip_prefix("DEP_")
        .and_then(|rest| rest.strip_suffix(key))
        .and_then(|rest| rest.strip_suffix('_'))
        .unwrap_or(var)
        .to_owned()
}

/// Every dependency's contract, sorted by the advertising crate's `links`
/// name so generated output does not depend on env-var order.
#[must_use]
pub fn collect_dep_contracts() -> Vec<Contract> {
    collect_keyed(CONTRACT_KEY, LEGACY_CONTRACT_KEY, deserialize_contract)
}

/// Every dependency's native deps merged in `links`-name order, so
/// conflict resolution is deterministic.
#[must_use]
pub fn collect_dep_native_deps() -> NativeDeps {
    let mut out = NativeDeps::new();
    for deps in collect_keyed(
        NATIVE_DEPS_KEY,
        LEGACY_NATIVE_DEPS_KEY,
        deserialize_native_deps,
    ) {
        out.merge(deps);
    }
    out
}

/// Decodes `DEP_<LINKS>_<key>` payloads, falling back to
/// `DEP_<LINKS>_<legacy>` for crates that only emit the old key.
fn collect_keyed<T>(
    key: &str,
    legacy: &str,
    decode: fn(&str) -> Result<T, HandoverError>,
) -> Vec<T> {
    let current = collect_env_payloads(key);
    let mut by_links: BTreeMap<String, T> = BTreeMap::new();
    for (var, payload) in &current {
        match decode(payload) {
            Ok(value) => {
                by_links.insert(links_name(var, key), value);
            }
            Err(err) => {
                println!("cargo::warning=istmo handover decode failed for {var}: {err}");
            }
        }
    }
    for (var, payload) in collect_env_payloads(legacy) {
        // `DEP_X_ISTMO_CONTRACT` also ends in `_CONTRACT`.
        if current.iter().any(|(v, _)| *v == var) {
            continue;
        }
        let links = links_name(&var, legacy);
        if by_links.contains_key(&links) {
            continue;
        }
        if let Ok(value) = decode(&payload) {
            by_links.insert(links, value);
        }
    }
    by_links.into_values().collect()
}

/// `DEP_*_<key>` variables, sorted by name.
fn collect_env_payloads(key: &str) -> Vec<(String, String)> {
    let suffix = format!("_{key}");
    let mut out: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("DEP_") && k.ends_with(&suffix))
        .collect();
    out.sort();
    out
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{byte:02X}"));
    }
    out
}

fn hex_decode(hex: &str) -> Result<Vec<u8>, HandoverError> {
    if !hex.len().is_multiple_of(2) {
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
        let err = deserialize_contract(hex).expect_err("decode should fail");
        assert!(matches!(err, HandoverError::Decode(_)));
    }

    #[test]
    fn deserialize_refuses_trailing_bytes() {
        let hex = serialize_contract(&sample_contract()).expect("serialize") + "00";
        let err = deserialize_contract(&hex).expect_err("trailing byte");
        assert!(matches!(err, HandoverError::Decode(_)));
    }

    #[test]
    fn links_name_strips_prefix_and_key() {
        assert_eq!(
            links_name("DEP_ISTMO_SHARE_ISTMO_MANIFEST", MANIFEST_KEY),
            "ISTMO_SHARE"
        );
        assert_eq!(
            links_name(
                "DEP_PEN_ISTMO_NATIVE_ANDROID",
                NativePlatform::Android.env_key()
            ),
            "PEN"
        );
    }

    #[test]
    fn hex_decode_accepts_both_cases() {
        let upper = "48454C4C4F";
        let lower = "48454c4c4f";
        assert_eq!(hex_decode(upper).unwrap(), b"HELLO");
        assert_eq!(hex_decode(lower).unwrap(), b"HELLO");
    }
}
