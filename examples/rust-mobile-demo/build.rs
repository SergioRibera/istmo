use istmo_build::AppOpts;
use istmo_plugins_schema as plugin_contract;

fn main() {
    // SignIn ships through the standard `DEP_*_ISTMO_CONTRACTS` handover
    // from `istmo-google-sign-in`'s own build.rs. Permissions/Notifications/
    // AdMob live in the shared `istmo-plugins-schema` crate (a build-time
    // crate that hands nothing over), so we pass them explicitly.
    istmo_build::emit_with(AppOpts {
        extra_contracts: vec![
            plugin_contract::permissions(),
            plugin_contract::notifications(),
            plugin_contract::admob(),
        ],
        ..AppOpts::default()
    });
}
