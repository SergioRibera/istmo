//! Contract handover between plugin build scripts and the app's:
//! the multi-contract payload, the single-contract keys older
//! `istmo-build` releases emit, and how duplicates resolve.

use istmo_build::{
    CONTRACT_KEY, CONTRACTS_KEY, Contract, Method, MethodKind, TypeRef, collect_contracts_from,
    deserialize_contracts, serialize_contract, serialize_contracts,
};

fn contract(plugin_id: &str, type_name: &str) -> Contract {
    Contract {
        plugin_id: plugin_id.to_owned(),
        type_name: type_name.to_owned(),
        methods: vec![Method {
            name: "ping".to_owned(),
            kind: MethodKind::Unary,
            args: vec![],
            returns: TypeRef::Option(Box::new(TypeRef::Vec(Box::new(TypeRef::String)))),
            error: None,
        }],
        init: None,
        types: vec![],
    }
}

fn var(links: &str, key: &str, payload: String) -> (String, String) {
    (format!("DEP_{links}_{key}"), payload)
}

fn multi(links: &str, contracts: &[Contract]) -> (String, String) {
    var(
        links,
        CONTRACTS_KEY,
        serialize_contracts(contracts).unwrap(),
    )
}

fn single(links: &str, contract: &Contract) -> (String, String) {
    var(links, CONTRACT_KEY, serialize_contract(contract).unwrap())
}

fn ids(contracts: &[Contract]) -> Vec<(&str, &str)> {
    contracts
        .iter()
        .map(|c| (c.plugin_id.as_str(), c.type_name.as_str()))
        .collect()
}

#[test]
fn multi_payload_round_trips() {
    let contracts = vec![contract("a.one", "One"), contract("a.two", "Two")];
    let hex = serialize_contracts(&contracts).unwrap();
    assert_eq!(deserialize_contracts(&hex).unwrap(), contracts);
    assert_eq!(
        deserialize_contracts(&serialize_contracts(&[]).unwrap()).unwrap(),
        []
    );
}

#[test]
fn multi_payload_refuses_trailing_bytes_and_bad_hex() {
    let hex = serialize_contracts(&[contract("a.one", "One")]).unwrap();
    assert!(deserialize_contracts(&format!("{hex}00")).is_err());
    assert!(deserialize_contracts("zz").is_err());
    assert!(deserialize_contracts("FFFFFFFFFF").is_err());
}

#[test]
fn every_contract_of_a_multi_payload_is_collected() {
    let got = collect_contracts_from([multi(
        "SCHEMA",
        &[contract("s.one", "One"), contract("s.two", "Two")],
    )]);
    assert_eq!(ids(&got), [("s.one", "One"), ("s.two", "Two")]);
}

#[test]
fn single_contract_keys_from_older_releases_are_still_collected() {
    let legacy = serialize_contract(&contract("l.bare", "Bare")).unwrap();
    let got = collect_contracts_from([
        single("OLD", &contract("o.one", "One")),
        var("BARE", "CONTRACT", legacy),
    ]);
    assert_eq!(ids(&got), [("l.bare", "Bare"), ("o.one", "One")]);
}

#[test]
fn old_and_new_dependencies_mix_in_links_order() {
    let got = collect_contracts_from([
        multi("ZETA", &[contract("z.one", "Z1"), contract("z.two", "Z2")]),
        single("ALPHA", &contract("a.one", "A1")),
        ("PATH".to_owned(), "/bin".to_owned()),
        ("DEP_OTHER_CONTRACT".to_owned(), "not ours".to_owned()),
    ]);
    assert_eq!(
        ids(&got),
        [("a.one", "A1"), ("z.one", "Z1"), ("z.two", "Z2")]
    );
}

#[test]
fn a_crate_emitting_both_keys_keeps_the_multi_payload_first() {
    let got = collect_contracts_from([
        single("MIXED", &contract("m.one", "Stale")),
        multi(
            "MIXED",
            &[contract("m.one", "Fresh"), contract("m.two", "Two")],
        ),
    ]);
    assert_eq!(ids(&got), [("m.one", "Fresh"), ("m.two", "Two")]);
}

#[test]
fn the_unprefixed_key_is_ignored_once_a_current_key_exists() {
    let legacy = serialize_contract(&contract("x.one", "Legacy")).unwrap();
    let got = collect_contracts_from([
        var("X", "CONTRACT", legacy),
        multi("X", &[contract("x.one", "Current")]),
    ]);
    assert_eq!(ids(&got), [("x.one", "Current")]);
}

#[test]
fn a_plugin_id_claimed_by_two_crates_keeps_the_first_by_links_name() {
    let got = collect_contracts_from([
        multi(
            "BETA",
            &[contract("dup.id", "FromBeta"), contract("b.own", "B")],
        ),
        single("ALPHA", &contract("dup.id", "FromAlpha")),
    ]);
    assert_eq!(ids(&got), [("dup.id", "FromAlpha"), ("b.own", "B")]);
}

#[test]
fn a_corrupt_payload_is_skipped_without_dropping_the_others() {
    let got = collect_contracts_from([
        var("BROKEN", CONTRACTS_KEY, "DEADBEEF".to_owned()),
        var("ODD", CONTRACT_KEY, "ABC".to_owned()),
        var("BARE", "CONTRACT", "not hex".to_owned()),
        multi("GOOD", &[contract("g.one", "One")]),
    ]);
    assert_eq!(ids(&got), [("g.one", "One")]);
}
