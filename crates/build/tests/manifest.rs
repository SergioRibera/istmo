use istmo_build::{
    Deployment, GradleScope, InfoPlistEntry, InfoPlistValue, Manifest, NativeDeps,
    deserialize_native_deps, resolve_wiring, serialize_native_deps,
};

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

    let hex_a = serialize_native_deps(&a.native_deps).expect("serialize a");
    let hex_b = serialize_native_deps(&b.native_deps).expect("serialize b");

    let mut aggregate = NativeDeps::new();
    aggregate.merge(deserialize_native_deps(&hex_a).expect("decode a"));
    aggregate.merge(deserialize_native_deps(&hex_b).expect("decode b"));

    let gradle: Vec<_> = aggregate.gradle_entries().collect();

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

    let gradle_fragment = aggregate.render_gradle();
    assert!(gradle_fragment.contains("googleid:1.1.2"));
    assert!(!gradle_fragment.contains("googleid:1.1.1"));
    assert!(gradle_fragment.contains("credentials:1.3.0"));
    assert!(gradle_fragment.contains("activity-ktx:1.9.0"));
}

#[test]
fn plugin_only_manifest_is_valid_and_empty() {
    let src = "[plugin]\nid = \"istmo.example.headless\"\n";
    let m = Manifest::parse(src).expect("parse");
    assert_eq!(m.primary_id(), "istmo.example.headless");
    assert_eq!(m.plugins.len(), 1);
    assert!(m.native_deps.is_empty());
}

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
    assert!(
        gradle
            .iter()
            .all(|g| g.scope == GradleScope::Implementation)
    );

    let hex = serialize_native_deps(&m.native_deps).expect("serialize");
    let back = deserialize_native_deps(&hex).expect("deserialize");
    assert_eq!(back, m.native_deps);
}

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
    assert_eq!(
        wiring.remote_clients,
        vec!["::istmo_heavy_ml::HeavyMlClient"]
    );
}

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
    assert!(
        app.plugins.is_empty(),
        "app manifest may omit plugin sections"
    );
    assert_eq!(app.remote_overrides.len(), 1);

    let wiring = resolve_wiring(&[plugin], Some(&app));
    assert!(wiring.local_clients.is_empty());
    assert_eq!(
        wiring.remote_clients,
        vec!["::istmo_compute::ComputeClient"]
    );
}

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
    assert_eq!(
        wiring.local_clients,
        vec!["::istmo_heavy_ml::HeavyMlClient"]
    );
    assert!(wiring.remote_clients.is_empty());
}

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

#[test]
fn windows_manifest_fragments_parse_and_survive_bincode_roundtrip() {
    let src = r#"
[plugin]
id = "istmo.pen"

[[windows_manifest_fragment]]
name = "dpi_aware"
xml = "<dpiAware>true/pm</dpiAware>"

[[windows_manifest_fragment]]
name = "common_controls"
xml = "<dependency/>"
"#;
    let m = Manifest::parse(src).expect("parse");
    assert_eq!(m.windows_manifest_fragments.len(), 2);
    assert_eq!(m.windows_manifest_fragments[0].name, "dpi_aware");
    assert!(
        m.windows_manifest_fragments[0]
            .xml
            .contains("<dpiAware>true/pm</dpiAware>")
    );
    assert_eq!(m.windows_manifest_fragments[1].name, "common_controls");

    let hex = istmo_build::handover::serialize_manifest(&m).expect("serialize");
    let back = istmo_build::handover::deserialize_manifest(&hex).expect("deserialize");
    assert_eq!(
        back.windows_manifest_fragments,
        m.windows_manifest_fragments
    );
}

#[test]
fn windows_manifest_fragment_unknown_key_rejected() {
    let src = r#"
[plugin]
id = "istmo.pen"

[[windows_manifest_fragment]]
name = "dpi_aware"
xml = "<a/>"
scope = "app"
"#;
    let err = Manifest::parse(src).expect_err("must fail");
    let msg = format!("{err}");
    assert!(msg.contains("windows_manifest_fragment.scope"), "got {msg}");
}

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

#[test]
fn parses_min_versions_table() {
    let m = Manifest::parse(
        r#"
[plugin]
id = "istmo.share"

[min_versions]
android = 24
ios = "15.0"
macos = "11"
windows = "10.0.17763"
"#,
    )
    .expect("parse");
    assert_eq!(m.min_versions.android, Some(24));
    assert_eq!(
        m.min_versions.ios,
        Some(istmo_build::OsVersion::new(15, 0, 0))
    );
    assert_eq!(
        m.min_versions.macos,
        Some(istmo_build::OsVersion::new(11, 0, 0))
    );
    assert_eq!(
        m.min_versions.windows,
        Some(istmo_build::OsVersion::new(10, 0, 17763))
    );
    let hex = istmo_build::serialize_manifest(&m).expect("serialize");
    let back = istmo_build::deserialize_manifest(&hex).expect("deserialize");
    assert_eq!(back.min_versions, m.min_versions);
}

#[test]
fn rejects_malformed_min_versions() {
    let err = Manifest::parse("[min_versions]\nios = \"fifteen\"\n").expect_err("invalid");
    assert!(err.to_string().contains("min_versions.ios"), "{err}");
    let err = Manifest::parse("[min_versions]\ntvos = \"1\"\n").expect_err("unknown");
    assert!(err.to_string().contains("min_versions.tvos"), "{err}");
}

#[test]
fn plugin_info_plist_parses_strings_and_bools_and_survives_bincode_roundtrip() {
    let src = r#"
[plugin]
id = "istmo.biometric"

[plugin.info_plist]
NSFaceIDUsageDescription = "Unlock <secrets> & more"
ITSAppUsesNonExemptEncryption = false
"#;
    let m = Manifest::parse(src).expect("parse");
    let entries = &m.plugins[0].info_plist;
    assert_eq!(
        entries,
        &vec![
            InfoPlistEntry {
                key: "NSFaceIDUsageDescription".to_owned(),
                value: InfoPlistValue::String("Unlock <secrets> & more".to_owned()),
            },
            InfoPlistEntry {
                key: "ITSAppUsesNonExemptEncryption".to_owned(),
                value: InfoPlistValue::Bool(false),
            },
        ]
    );
    assert_eq!(
        entries[0].render(),
        "<key>NSFaceIDUsageDescription</key>\n<string>Unlock &lt;secrets&gt; &amp; more</string>\n"
    );
    assert_eq!(
        entries[1].render(),
        "<key>ITSAppUsesNonExemptEncryption</key>\n<false/>\n"
    );

    let hex = istmo_build::handover::serialize_manifest(&m).expect("serialize");
    let back = istmo_build::handover::deserialize_manifest(&hex).expect("deserialize");
    assert_eq!(back.plugins[0].info_plist, m.plugins[0].info_plist);
}

#[test]
fn plugin_info_plist_rejects_non_scalar_values() {
    let src = r#"
[plugin]
id = "istmo.biometric"

[plugin.info_plist]
NSFaceIDUsageDescription = 42
"#;
    let err = Manifest::parse(src).expect_err("must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("plugin.info_plist.NSFaceIDUsageDescription"),
        "got {msg}"
    );
}

#[test]
fn parses_backend_overrides() {
    use istmo_build::AndroidBackendArg;
    let manifest = Manifest::parse(
        "[plugin]\nid = \"istmo.biometric\"\n\
         android_backend = \"dev.istmo.plugins.biometric.BiometricBackendImpl\"\n\
         android_backend_arg = \"androidx.fragment.app.FragmentActivity\"\n\
         ios_backend = \"BiometricBackendImpl\"\n",
    )
    .unwrap();
    let plugin = &manifest.plugins[0];
    assert_eq!(
        plugin.android_backend.as_deref(),
        Some("dev.istmo.plugins.biometric.BiometricBackendImpl")
    );
    assert_eq!(
        plugin.android_backend_arg,
        AndroidBackendArg::Activity("androidx.fragment.app.FragmentActivity".to_owned())
    );
    assert_eq!(plugin.ios_backend.as_deref(), Some("BiometricBackendImpl"));

    let activity =
        Manifest::parse("[plugin]\nid = \"a\"\nandroid_backend_arg = \"activity\"\n").unwrap();
    assert_eq!(
        activity.plugins[0].android_backend_arg,
        AndroidBackendArg::Activity("androidx.activity.ComponentActivity".to_owned())
    );
    assert!(Manifest::parse("[plugin]\nid = \"a\"\nandroid_backend_arg = \"fragment\"\n").is_err());
}
