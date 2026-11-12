//! The hand-written contracts in `istmo-plugins-schema` drive the
//! Kotlin / Swift codegen; the `#[istmo::plugin]` traits here drive the
//! Rust side of the wire. These tests derive a contract from each trait's
//! source and hold the schema to it, so neither side drifts alone.

use std::path::{Path, PathBuf};

use istmo_build::{Contract, TypeDef, extract_contract};
use istmo_core::Plugin;
use istmo_plugins::{
    ADMOB_PLUGIN_ID, AdMobClient, NOTIFICATIONS_PLUGIN_ID, NotificationsClient,
    PERMISSIONS_PLUGIN_ID, PermissionsClient, SERVICE_CONTROL_PLUGIN_ID, ServiceControlClient,
    TASK_SCHEDULER_PLUGIN_ID, TaskSchedulerClient,
};

fn src(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file)
}

/// Contract of `trait_name` with the message types of every file in
/// `files`, in order. The extractor reads one file, so the sources are
/// concatenated first, minus their inner `//!` docs (only valid at the
/// top of a file).
fn extract(files: &[&str], trait_name: &str) -> Contract {
    let joined: String = files
        .iter()
        .map(|file| std::fs::read_to_string(src(file)).expect("read plugin source"))
        .flat_map(|source| {
            source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//!"))
                .map(|line| format!("{line}\n"))
                .collect::<Vec<_>>()
        })
        .collect();
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{trait_name}.rs"));
    std::fs::write(&path, joined).expect("write joined source");
    extract_contract(&path, trait_name).expect("extract contract")
}

/// Every schema type must match the extracted one. With
/// `types_in_source == false` the Rust types are generated from the
/// schema itself (`include!` from `build.rs`), so they cannot drift and
/// the extractor does not see them.
fn assert_same(schema: &Contract, extracted: &Contract, types_in_source: bool) {
    assert_eq!(schema.plugin_id, extracted.plugin_id);
    assert_eq!(schema.type_name, extracted.type_name);
    assert_eq!(schema.init, extracted.init, "{}: init", schema.type_name);
    assert_eq!(
        schema.methods, extracted.methods,
        "{}: methods",
        schema.type_name
    );
    if !types_in_source {
        return;
    }
    for ty in &schema.types {
        let found: Option<&TypeDef> = extracted.types.iter().find(|t| t.name() == ty.name());
        assert_eq!(
            Some(ty),
            found,
            "{}: type `{}`",
            schema.type_name,
            ty.name()
        );
    }
}

#[test]
fn service_control_schema_matches_the_trait() {
    assert_same(
        &istmo_plugins_schema::service_control(),
        &extract(&["service.rs"], "ServiceControl"),
        true,
    );
}

#[test]
fn task_scheduler_schema_matches_the_trait() {
    assert_same(
        &istmo_plugins_schema::task_scheduler(),
        &extract(&["worker.rs", "task_scheduler.rs"], "TaskScheduler"),
        true,
    );
}

#[test]
fn permissions_schema_matches_the_trait() {
    assert_same(
        &istmo_plugins_schema::permissions(),
        &extract(&["permissions.rs"], "Permissions"),
        false,
    );
}

#[test]
fn notifications_schema_matches_the_trait() {
    assert_same(
        &istmo_plugins_schema::notifications(),
        &extract(&["notifications.rs"], "Notifications"),
        false,
    );
}

#[test]
fn admob_schema_matches_the_trait() {
    assert_same(
        &istmo_plugins_schema::admob(),
        &extract(&["admob/mod.rs"], "AdMob"),
        false,
    );
}

/// `#[plugin(name = …)]` takes a literal, so the exported id constants
/// are a second copy; keep them equal to what the macro emits.
#[test]
fn exported_plugin_ids_match_the_generated_clients() {
    let pairs = [
        (ADMOB_PLUGIN_ID, AdMobClient::PLUGIN_ID),
        (NOTIFICATIONS_PLUGIN_ID, NotificationsClient::PLUGIN_ID),
        (PERMISSIONS_PLUGIN_ID, PermissionsClient::PLUGIN_ID),
        (SERVICE_CONTROL_PLUGIN_ID, ServiceControlClient::PLUGIN_ID),
        (TASK_SCHEDULER_PLUGIN_ID, TaskSchedulerClient::PLUGIN_ID),
        (
            istmo_plugins::ACTIVITY_RESULTS_PLUGIN_ID,
            istmo_plugins::ActivityResultsClient::PLUGIN_ID,
        ),
    ];
    for (exported, generated) in pairs {
        assert_eq!(exported, generated);
    }
    for (schema, generated) in [
        (istmo_plugins_schema::admob(), ADMOB_PLUGIN_ID),
        (
            istmo_plugins_schema::notifications(),
            NOTIFICATIONS_PLUGIN_ID,
        ),
        (istmo_plugins_schema::permissions(), PERMISSIONS_PLUGIN_ID),
        (
            istmo_plugins_schema::service_control(),
            SERVICE_CONTROL_PLUGIN_ID,
        ),
        (
            istmo_plugins_schema::task_scheduler(),
            TASK_SCHEDULER_PLUGIN_ID,
        ),
    ] {
        assert_eq!(schema.plugin_id, generated);
    }
}
