//! End-to-end coverage for `istmo.toml` parsing plus the cross-plugin
//! aggregation channel that a real downstream `build.rs` would exercise.
//!
//! The plugin side of the handover is a `println!("cargo::metadata::…")` we
//! can't intercept from inside a test, so the test simulates the pipeline
//! at the level Cargo would: each plugin's [`NativeDeps`] contribution is
//! bincode-encoded (same serializer the emit / collect helpers use) and
//! decoded back on the aggregating side, then merged via [`NativeDeps::merge`].
//!
//! This proves two invariants at once:
//!
//! * The manifest parser produces bundles equivalent to what today's
//!   hand-authored `NativeDeps::new().add_gradle(...)` calls produce.
//! * The version-conflict policy documented in `native_deps.rs` (highest
//!   wins + one [`VersionConflict`] per losing candidate) still holds when
//!   the inputs come from `istmo.toml` files.

use istmo_build::{
    Deployment, GradleScope, Manifest, NativeDeps, deserialize_native_deps, resolve_wiring,
    serialize_native_deps,
};

/// Two plugin authors publish the same `googleid` artefact at different
/// versions. Aggregation picks 1.1.2 (higher) and records the older one as
/// a conflict for the app author's `cargo:warning=` line.
#[test]
fn aggregates_two_manifests_via_bincode_channel() {
    let sign_in_toml = r#"
[plugin]
id = "istmo.google_sign_in"

[[gradle]]
group = "androidx.credentials"
artifact = "credentials"
version = "1.3.0"

[[gradle]]
scope = "api"
group = "com.google.android.libraries.identity.googleid"
artifact = "googleid"
version = "1.1.1"

[[swift_package]]
url = "https://github.com/google/GoogleSignIn-iOS.git"
product = "GoogleSignIn"
from_version = "7.0.0"
"#;
    let other_plugin_toml = r#"
[plugin]
id = "istmo.example.other"

[[gradle]]
scope = "api"
group = "com.google.android.libraries.identity.googleid"
artifact = "googleid"
version = "1.1.2"

[[gradle]]
group = "androidx.activity"
artifact = "activity-ktx"
version = "1.9.0"
"#;

    let a = Manifest::parse(sign_in_toml).expect("parse sign-in manifest");
    let b = Manifest::parse(other_plugin_toml).expect("parse other manifest");

    // Serialize each contribution the way `emit_native_deps` does.
    let hex_a = serialize_native_deps(&a.native_deps).expect("serialize a");
    let hex_b = serialize_native_deps(&b.native_deps).expect("serialize b");

    // Aggregating build.rs decodes each and merges.
    let mut aggregate = NativeDeps::new();
    aggregate.merge(deserialize_native_deps(&hex_a).expect("decode a"));
    aggregate.merge(deserialize_native_deps(&hex_b).expect("decode b"));

    let gradle: Vec<_> = aggregate.gradle_entries().collect();
    // 3 unique `(scope, group, artifact)` tuples once the shared
    // (api, googleid) row collapses via `merge`.
    assert_eq!(gradle.len(), 3);

    let googleid = gradle
        .iter()
        .find(|d| {
            d.coord.group == "com.google.android.libraries.identity.googleid"
                && d.coord.artifact == "googleid"
        })
        .expect("googleid entry");
    assert_eq!(googleid.coord.version, "1.1.2", "highest version wins");
    assert_eq!(googleid.scope, GradleScope::Api);

    let conflicts = aggregate.conflicts();
    assert_eq!(conflicts.len(), 1, "one conflict recorded");
    assert_eq!(conflicts[0].picked, "1.1.2");
    assert_eq!(conflicts[0].discarded, vec!["1.1.1".to_owned()]);

    let swift: Vec<_> = aggregate.swift_entries().collect();
    assert_eq!(swift.len(), 1);
    assert_eq!(swift[0].product, "GoogleSignIn");

    // Rendering keeps the aggregate deterministic and lands the higher
    // googleid version — sanity check for the app author's Gradle fragment.
    let gradle_fragment = aggregate.render_gradle();
    assert!(gradle_fragment.contains("googleid:1.1.2"));
    assert!(!gradle_fragment.contains("googleid:1.1.1"));
    assert!(gradle_fragment.contains("credentials:1.3.0"));
    assert!(gradle_fragment.contains("activity-ktx:1.9.0"));
}

/// Sanity: a manifest with only `[plugin]` is legal (declares plugin id
/// without contributing native deps) and yields an empty bundle.
#[test]
fn plugin_only_manifest_is_valid_and_empty() {
    let src = "[plugin]\nid = \"istmo.example.headless\"\n";
    let m = Manifest::parse(src).expect("parse");
    assert_eq!(m.primary_id(), "istmo.example.headless");
    assert_eq!(m.plugins.len(), 1);
    assert!(m.native_deps.is_empty());
}

/// A single crate exposes two plugins with nested and shared native deps.
/// Downstream aggregation sees the merged bundle, and the metadata channel
/// carries both ids in the `PLUGIN_IDS` list.
#[test]
fn aggregates_multi_plugin_crate() {
    let src = r#"
[[plugin]]
id = "istmo.google_sign_in"
client_type = "::istmo_plugins::SignInClient"

  [[plugin.gradle]]
  group = "androidx.credentials"
  artifact = "credentials"
  version = "1.3.0"

[[plugin]]
id = "istmo.admob"

  [[plugin.gradle]]
  group = "com.google.android.gms"
  artifact = "play-services-ads"
  version = "23.0.0"

[[gradle]]
group = "androidx.core"
artifact = "core-ktx"
version = "1.13.0"
"#;
    let m = Manifest::parse(src).expect("parse");
    let ids: Vec<_> = m.plugin_ids().collect();
    assert_eq!(ids, vec!["istmo.google_sign_in", "istmo.admob"]);
    let gradle: Vec<_> = m.native_deps.gradle_entries().collect();
    assert_eq!(gradle.len(), 3);
    assert!(gradle.iter().all(|g| g.scope == GradleScope::Implementation));
    // Same env-var channel a downstream build.rs would use.
    let hex = serialize_native_deps(&m.native_deps).expect("serialize");
    let back = deserialize_native_deps(&hex).expect("deserialize");
    assert_eq!(back, m.native_deps);
}

/// Plugin manifest can hint deployment via `default_deployment = "remote"`.
/// Parser accepts the field; `resolve_wiring` picks it up when no override
/// is present.
#[test]
fn manifest_default_deployment_remote_flows_into_wiring() {
    let plugin_src = r#"
[plugin]
id = "istmo.heavy_ml"
client_type = "::istmo_heavy_ml::HeavyMlClient"
default_deployment = "remote"
"#;
    let plugin = Manifest::parse(plugin_src).expect("parse plugin manifest");
    assert_eq!(plugin.plugins[0].default_deployment, Deployment::Remote);

    let wiring = resolve_wiring(&[plugin], None);
    assert!(wiring.local_clients.is_empty());
    assert_eq!(wiring.remote_clients, vec!["::istmo_heavy_ml::HeavyMlClient"]);
}

/// Local-by-default plugin flipped to remote via app-side override.
#[test]
fn app_manifest_override_flips_local_plugin_to_remote() {
    let plugin_src = r#"
[plugin]
id = "istmo.compute"
client_type = "::istmo_compute::ComputeClient"
"#;
    let app_src = r#"
[[remote_override]]
plugin = "istmo.compute"
deployment = "remote"
"#;
    let plugin = Manifest::parse(plugin_src).expect("parse plugin");
    let app = Manifest::parse(app_src).expect("parse app");
    assert!(app.plugins.is_empty(), "app manifest may omit plugin sections");
    assert_eq!(app.remote_overrides.len(), 1);

    let wiring = resolve_wiring(&[plugin], Some(&app));
    assert!(wiring.local_clients.is_empty());
    assert_eq!(wiring.remote_clients, vec!["::istmo_compute::ComputeClient"]);
}

/// Reverse override — plugin declares itself remote, app forces local.
#[test]
fn app_manifest_override_flips_remote_plugin_to_local() {
    let plugin_src = r#"
[plugin]
id = "istmo.heavy_ml"
client_type = "::istmo_heavy_ml::HeavyMlClient"
default_deployment = "remote"
"#;
    let app_src = r#"
[[remote_override]]
plugin = "istmo.heavy_ml"
deployment = "local"
"#;
    let plugin = Manifest::parse(plugin_src).expect("parse plugin");
    let app = Manifest::parse(app_src).expect("parse app");

    let wiring = resolve_wiring(&[plugin], Some(&app));
    assert_eq!(wiring.local_clients, vec!["::istmo_heavy_ml::HeavyMlClient"]);
    assert!(wiring.remote_clients.is_empty());
}

/// Plugin without `client_type` contributes native deps only — skipped in
/// wiring.
#[test]
fn plugin_without_client_type_is_skipped_by_wiring() {
    let plugin_src = r#"
[plugin]
id = "istmo.native_only"

[[gradle]]
group = "androidx.core"
artifact = "core-ktx"
version = "1.13.0"
"#;
    let plugin = Manifest::parse(plugin_src).expect("parse");
    let wiring = resolve_wiring(&[plugin], None);
    assert!(wiring.local_clients.is_empty());
    assert!(wiring.remote_clients.is_empty());
}

/// Unknown `deployment` literal → parse error.
#[test]
fn unknown_deployment_literal_is_rejected() {
    let src = r#"
[plugin]
id = "istmo.example"
default_deployment = "sandbox"
"#;
    let err = Manifest::parse(src).expect_err("must fail");
    let msg = format!("{err}");
    assert!(msg.contains("`sandbox`"), "got {msg}");
    assert!(msg.contains("expected `local` or `remote`"), "got {msg}");
}
