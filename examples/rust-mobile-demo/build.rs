use istmo_build::AppOpts;
use istmo_plugins_schema as plugin_contract;

fn main() {
    // SignIn and Notifications ship through the standard
    // `DEP_*_ISTMO_CONTRACTS` handover from their plugin crates' build.rs.
    // Permissions / AdMob live in the shared `istmo-plugins-schema` crate
    // (a build-time crate that hands nothing over), so we pass them
    // explicitly.
    istmo_build::emit_with(AppOpts {
        extra_contracts: vec![plugin_contract::permissions(), plugin_contract::admob()],
        ..AppOpts::default()
    });
}
