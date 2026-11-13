//! Build-script handover payloads, including the hand-written recursive
//! `TypeRef` decoder and its depth guard.
#![no_main]

use istmo_build::handover::{
    deserialize_assets, deserialize_contract, deserialize_contracts, deserialize_manifest,
    deserialize_native_deps, serialize_contract, serialize_contracts,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let hex: String = data.iter().map(|b| format!("{b:02x}")).collect();
    if let Ok(contract) = deserialize_contract(&hex) {
        let again = serialize_contract(&contract).expect("a decoded contract encodes");
        assert_eq!(deserialize_contract(&again).expect("round trip"), contract);
    }
    if let Ok(contracts) = deserialize_contracts(&hex) {
        let again = serialize_contracts(&contracts).expect("decoded contracts encode");
        assert_eq!(
            deserialize_contracts(&again).expect("round trip"),
            contracts
        );
    }
    let _ = deserialize_manifest(&hex);
    let _ = deserialize_native_deps(&hex);
    let _ = deserialize_assets(&hex);
});
