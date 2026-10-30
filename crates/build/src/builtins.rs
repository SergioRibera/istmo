//! Registry of first-party plugin IDs shipped by `istmo-plugins`
//! together with their platform permission footprint.
//!
//! Builtins live in a shared crate rather than per-plugin crates, so
//! they do not travel through the `DEP_*_ISTMO_MANIFEST` handover like
//! standalone plugin crates do. This module centralises the metadata
//! an app build needs to decide which `<uses-permission>` and
//! `Info.plist` entries to inject when a builtin is actually wired via
//! `istmo::runtime!(plugins: [...])`.
//!
//! Consumers obtain the full set with [`entries`] and filter by the
//! plugin IDs resolved from the app's source. Entries with empty
//! `android_permissions` and `info_plist` are still listed so a
//! downstream filter can report "known builtin, nothing to inject"
//! without a second lookup table.

use crate::manifest::PluginEntry;

/// Returns one [`PluginEntry`] per first-party plugin bundled with
/// `istmo-plugins`.
///
/// Only `id`, `client_type`, `android_permissions` and `info_plist`
/// are meaningful; the remaining fields are set to their
/// [`PluginEntry`] defaults.
#[must_use]
pub fn entries() -> Vec<PluginEntry> {
    [
        ("istmo.lifecycle", "::istmo_plugins::AppLifecycle", &[][..]),
        ("istmo.deeplinks", "::istmo_plugins::DeepLinks", &[][..]),
        ("istmo.safe_area", "::istmo_plugins::SafeArea", &[][..]),
        ("istmo.system_info", "::istmo_plugins::SystemInfo", &[][..]),
        (
            "istmo.device_info",
            "::istmo_plugins::DeviceInfoClient",
            &[][..],
        ),
        (
            "istmo.theme_preference",
            "::istmo_plugins::ThemePreferenceClient",
            &[][..],
        ),
        (
            "istmo.network_state",
            "::istmo_plugins::NetworkStateClient",
            &["android.permission.ACCESS_NETWORK_STATE"][..],
        ),
        (
            "istmo.battery_state",
            "::istmo_plugins::BatteryStateClient",
            &[][..],
        ),
        (
            "istmo.storage_info",
            "::istmo_plugins::StorageInfoClient",
            &[][..],
        ),
        (
            "istmo.capabilities",
            "::istmo_plugins::CapabilitiesClient",
            &[][..],
        ),
    ]
    .into_iter()
    .map(|(id, client_type, perms)| PluginEntry {
        id: id.to_owned(),
        client_type: Some(client_type.to_owned()),
        android_permissions: perms.iter().map(|&s| s.to_owned()).collect(),
        ..PluginEntry::default()
    })
    .collect()
}

/// Lookup a builtin entry by its plugin ID.
#[must_use]
pub fn find(plugin_id: &str) -> Option<PluginEntry> {
    entries().into_iter().find(|e| e.id == plugin_id)
}

/// Lookup a builtin entry by its fully-qualified client type path.
///
/// Accepts both the `::`-rooted form
/// (`::istmo_plugins::NetworkStateClient`) and the shorter
/// `istmo_plugins::X` form.
#[must_use]
pub fn find_by_client_type(path: &str) -> Option<PluginEntry> {
    let normalised = path.trim_start_matches("::");
    entries().into_iter().find(|e| {
        e.client_type
            .as_deref()
            .is_some_and(|ct| ct.trim_start_matches("::") == normalised)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_state_declares_access_network_state() {
        let entry = find("istmo.network_state").expect("registered");
        assert!(
            entry
                .android_permissions
                .iter()
                .any(|p| p == "android.permission.ACCESS_NETWORK_STATE")
        );
    }

    #[test]
    fn battery_state_has_no_android_permissions() {
        let entry = find("istmo.battery_state").expect("registered");
        assert!(entry.android_permissions.is_empty());
    }

    #[test]
    fn lookup_by_client_type_handles_both_forms() {
        assert_eq!(
            find_by_client_type("::istmo_plugins::NetworkStateClient").map(|e| e.id),
            Some("istmo.network_state".to_owned())
        );
        assert_eq!(
            find_by_client_type("istmo_plugins::NetworkStateClient").map(|e| e.id),
            Some("istmo.network_state".to_owned())
        );
    }

    #[test]
    fn every_entry_has_unique_id() {
        let all = entries();
        let mut ids: Vec<_> = all.iter().map(|e| e.id.clone()).collect();
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate builtin plugin ids");
    }
}
