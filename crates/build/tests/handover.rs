//! Contract handover between plugin build scripts and the app's: the
//! `ISTMO_CONTRACTS` payload and how duplicates and broken payloads
//! resolve.

use istmo_build::{
    CONTRACTS_KEY, Contract, Method, MethodKind, TypeRef, collect_contracts_from,
    contracts_handover_line, deserialize_contracts, serialize_contracts,
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
}

#[test]
fn multi_payload_refuses_trailing_bytes_and_bad_hex() {
    let hex = serialize_contracts(&[contract("a.one", "One")]).unwrap();
    assert!(deserialize_contracts(&format!("{hex}00")).is_err());
    assert!(deserialize_contracts("zz").is_err());
    assert!(deserialize_contracts("FFFFFFFFFF").is_err());
}

#[test]
fn handover_line_is_one_cargo_directive_for_every_contract() {
    let contracts = [contract("s.one", "One"), contract("s.two", "Two")];
    let line = contracts_handover_line(&contracts).unwrap();
    let payload = line
        .strip_prefix("cargo:ISTMO_CONTRACTS=")
        .expect("`cargo:ISTMO_CONTRACTS=` prefix");
    assert!(!payload.contains(['\n', '=']));
    assert_eq!(payload, serialize_contracts(&contracts).unwrap());
}

#[test]
fn every_contract_of_a_payload_is_collected() {
    let got = collect_contracts_from([multi(
        "SCHEMA",
        &[contract("s.one", "One"), contract("s.two", "Two")],
    )]);
    assert_eq!(ids(&got), [("s.one", "One"), ("s.two", "Two")]);
}

#[test]
fn crates_are_collected_in_links_order_ignoring_other_variables() {
    let got = collect_contracts_from([
        multi("ZETA", &[contract("z.one", "Z1"), contract("z.two", "Z2")]),
        ("PATH".to_owned(), "/bin".to_owned()),
        multi("ALPHA", &[contract("a.one", "A1")]),
        ("DEP_OTHER_CONTRACT".to_owned(), "not ours".to_owned()),
        ("DEP_OTHER_ISTMO_CONTRACT".to_owned(), "not ours".to_owned()),
        (
            "DEP__ISTMO_CONTRACTS".to_owned(),
            "no links name".to_owned(),
        ),
    ]);
    assert_eq!(
        ids(&got),
        [("a.one", "A1"), ("z.one", "Z1"), ("z.two", "Z2")]
    );
}

#[test]
fn an_empty_payload_contributes_nothing() {
    let got = collect_contracts_from([
        multi("EMPTY", &[]),
        multi("FULL", &[contract("f.one", "One")]),
    ]);
    assert_eq!(ids(&got), [("f.one", "One")]);
}

#[test]
fn a_plugin_id_repeated_inside_one_crate_keeps_the_first() {
    let got = collect_contracts_from([multi(
        "SAME",
        &[
            contract("s.dup", "First"),
            contract("s.other", "Other"),
            contract("s.dup", "Second"),
        ],
    )]);
    assert_eq!(ids(&got), [("s.dup", "First"), ("s.other", "Other")]);
}

#[test]
fn a_plugin_id_claimed_by_two_crates_keeps_the_first_by_links_name() {
    let got = collect_contracts_from([
        multi(
            "BETA",
            &[contract("dup.id", "FromBeta"), contract("b.own", "B")],
        ),
        multi("ALPHA", &[contract("dup.id", "FromAlpha")]),
    ]);
    assert_eq!(ids(&got), [("dup.id", "FromAlpha"), ("b.own", "B")]);
}

#[test]
fn a_corrupt_payload_is_skipped_without_dropping_the_others() {
    let got = collect_contracts_from([
        var("BROKEN", CONTRACTS_KEY, "DEADBEEF".to_owned()),
        var("ODD", CONTRACTS_KEY, "ABC".to_owned()),
        var("TEXT", CONTRACTS_KEY, "not hex".to_owned()),
        multi("GOOD", &[contract("g.one", "One")]),
    ]);
    assert_eq!(ids(&got), [("g.one", "One")]);
}
