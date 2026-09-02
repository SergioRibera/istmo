//! Demo of the `permissions` core plugin using the settled mock pattern.
//!
//! Run with `cargo run --example permissions`.

use istmo::Runtime;
use istmo::plugins::{
    PermissionOutcome, PermissionStatus, Permissions, PermissionsClient, PermissionsHost,
};

#[derive(Debug, Default)]
pub struct MockPermissions;

impl Permissions for MockPermissions {
    async fn check(&self, permission: String) -> PermissionStatus {
        println!("mock check({permission}) — returning NotDetermined");
        PermissionStatus::NotDetermined
    }

    async fn request(&self, permissions: Vec<String>) -> Vec<PermissionOutcome> {
        permissions
            .into_iter()
            .map(|permission| PermissionOutcome {
                permission,
                status: PermissionStatus::Granted,
            })
            .collect()
    }

    async fn should_show_rationale(&self, _permission: String) -> bool {
        true
    }
}

fn main() {
    let init = Runtime::mock()
        .expects::<PermissionsClient>()
        .host(PermissionsHost::new(MockPermissions))
        .finish();

    let plugin = PermissionsClient::from_runtime(&init.runtime).expect("declared");

    let camera = pollster::block_on(plugin.check("android.permission.CAMERA".to_owned()))
        .expect("check camera");
    println!("check(CAMERA) -> {camera:?}");

    let outcomes = pollster::block_on(plugin.request(vec![
        "android.permission.CAMERA".to_owned(),
        "android.permission.RECORD_AUDIO".to_owned(),
    ]))
    .expect("request");
    for outcome in outcomes {
        println!(
            "request outcome: {} -> {:?}",
            outcome.permission, outcome.status,
        );
    }

    let show =
        pollster::block_on(plugin.should_show_rationale("android.permission.CAMERA".to_owned()))
            .expect("rationale");
    println!("should_show_rationale(CAMERA) -> {show}");
}
