//! The `hosts:` line the notifications docs show compiles as written:
//! the section names the trait (`Notifications`), not the client.
#![cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]

use istmo::Runtime;
use istmo_notifications::{DesktopNotifications, NotificationsClient};

fn host() -> DesktopNotifications {
    DesktopNotifications::platform()
        .with_channels(istmo_notifications::static_channels!())
        .with_sound_dir(istmo_notifications::bundled_sounds!())
        .without_store()
        .build()
}

istmo::runtime!(
    plugins: [NotificationsClient],
    hosts: [istmo_notifications::Notifications => host()],
);

#[test]
fn the_documented_hosts_line_wires_the_desktop_host() {
    let init = __istmo_configure_runtime(Runtime::mock());
    let client = NotificationsClient::from_runtime(&init.runtime).expect("declared");
    assert!(client.is_local(), "served in-process");
}
