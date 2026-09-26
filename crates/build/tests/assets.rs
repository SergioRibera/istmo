//! End-to-end coverage for the asset pipeline.
//!
//! Exercises the manifest → glob resolution → bincode round-trip path
//! that plugin `build.rs` scripts follow when advertising bundled
//! assets through the `DEP_*_ISTMO_ASSETS` env-var handover.

use std::fs;
use std::path::PathBuf;

use istmo_build::{
    AssetEntry, AssetsPayload, Manifest, ResolvedAsset, deserialize_assets, resolve_assets,
    serialize_assets,
};

fn tmp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("istmo-build-assets-{name}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn manifest_assets_survive_resolve_and_wire_round_trip() {
    let root = tmp_root("round-trip");
    fs::create_dir_all(root.join("assets/img/icons")).unwrap();
    fs::write(root.join("assets/img/logo.png"), b"png-logo").unwrap();
    fs::write(root.join("assets/img/icons/gear.png"), b"png-gear").unwrap();
    fs::write(root.join("assets/schema.sql"), b"select 1").unwrap();
    let manifest_toml = r#"
[plugin]
id = "istmo.example"

[[assets]]
path = "assets/img/"

[[assets]]
path = "assets/schema.sql"
"#;
    let manifest = Manifest::parse(manifest_toml).expect("parse manifest");
    assert_eq!(manifest.assets.len(), 2);

    let resolved = resolve_assets(&manifest.assets, &root).expect("resolve");
    let keys: Vec<&str> = resolved.iter().map(|r| r.bundle_path.as_str()).collect();
    assert_eq!(keys, ["img/icons/gear.png", "img/logo.png", "schema.sql"]);

    let payload = AssetsPayload {
        plugin_ids: manifest.plugin_ids().map(str::to_owned).collect(),
        resolved,
    };
    let hex = serialize_assets(&payload).expect("serialize");
    let back = deserialize_assets(&hex).expect("deserialize");
    assert_eq!(back, payload);
}

#[test]
fn glob_matches_only_declared_extensions() {
    let root = tmp_root("glob-ext");
    fs::create_dir_all(root.join("assets/audio")).unwrap();
    fs::write(root.join("assets/audio/tap.wav"), b"wav").unwrap();
    fs::write(root.join("assets/audio/click.mp3"), b"mp3").unwrap();
    fs::write(root.join("assets/audio/hold.wav"), b"wav").unwrap();
    let entries = vec![AssetEntry {
        path: "assets/**/*.wav".to_owned(),
    }];
    let resolved = resolve_assets(&entries, &root).expect("resolve");
    let keys: Vec<&str> = resolved.iter().map(|r| r.bundle_path.as_str()).collect();
    assert_eq!(keys, ["audio/hold.wav", "audio/tap.wav"]);
}

#[test]
fn resolved_asset_source_paths_are_absolute() {
    let root = tmp_root("abs");
    fs::create_dir_all(root.join("assets")).unwrap();
    fs::write(root.join("assets/data.bin"), b"01").unwrap();
    let entries = vec![AssetEntry {
        path: "assets/data.bin".to_owned(),
    }];
    let resolved = resolve_assets(&entries, &root).expect("resolve");
    assert_eq!(resolved.len(), 1);
    let ResolvedAsset {
        bundle_path,
        source_abs,
    } = &resolved[0];
    assert_eq!(bundle_path, "data.bin");
    assert!(source_abs.is_absolute());
}
