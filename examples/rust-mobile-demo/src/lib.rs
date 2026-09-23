use istmo::plugins::{NotificationsClient, PermissionsClient, SafeArea};
use istmo_plugins::admob::AdMobClient;

istmo::runtime!(
    plugins: [PermissionsClient, NotificationsClient, AdMobClient, SafeArea],
);

#[cfg(any(
    target_os = "android",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "visionos",
))]
mod app;
