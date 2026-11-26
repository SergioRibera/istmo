//! Public desktop surface: XDG autostart round trip (Linux), marker
//! stripping, the launch-reason lane and the unavailable host.

#![cfg(not(any(target_os = "android", target_os = "ios")))]

use std::ffi::OsString;
use std::sync::Arc;

use istmo_autostart::AutostartClient;
use istmo_autostart::{
    Autostart, AutostartConfig, AutostartError, AutostartOptions, AutostartPublisher,
    AutostartState, Detection, LaunchReason, LaunchReasonClient, UnavailableAutostart,
};
use istmo_core::Runtime;

fn runtime() -> Arc<Runtime> {
    Runtime::mock()
        .expects::<AutostartClient>()
        .finish()
        .runtime
}

#[test]
fn unavailable_host_reports_unsupported() {
    let host = UnavailableAutostart;
    assert_eq!(
        pollster::block_on(host.status()),
        Ok(AutostartState::Unsupported)
    );
    assert_eq!(
        pollster::block_on(host.enable(AutostartOptions::default())),
        Err(AutostartError::Unsupported)
    );
    assert_eq!(
        pollster::block_on(host.open_settings()),
        Err(AutostartError::Unsupported)
    );
}

#[test]
fn install_strips_the_marker_and_publishes_a_login_start() {
    let rt = runtime();
    let config = AutostartConfig::new("com.example.notes").with_marker("--autostart");
    let client = LaunchReasonClient::from_runtime(&rt).unwrap();
    assert_eq!(client.current().unwrap(), None);
    let args = AutostartPublisher::install(
        &rt,
        &config,
        ["--autostart", "notes.txt"].map(OsString::from),
    )
    .unwrap();
    assert_eq!(args, [OsString::from("notes.txt")]);
    // macOS publishes once `NSApplication` finishes launching; with the
    // marker present it does not wait.
    let reason = client.stream().recv().unwrap();
    assert!(
        matches!(
            reason,
            LaunchReason::Login(Detection::Marker | Detection::System)
        ),
        "{reason:?}"
    );
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::{Path, PathBuf};

    use super::*;
    use istmo_autostart::DesktopAutostart;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("istmo-autostart-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn host(config_home: &Path, exe: &str) -> DesktopAutostart {
        DesktopAutostart::new(
            AutostartConfig::new("com.example.notes")
                .with_marker("--autostart")
                .with_display_name("Notes"),
        )
        .with_xdg_config_home(config_home)
        .with_executable(exe)
    }

    #[test]
    fn desktop_entry_round_trip() {
        let dir = TempDir::new("round-trip");
        let host = host(&dir.0, "/opt/My Notes/notes");
        let entry = dir.0.join("autostart/com.example.notes.desktop");

        assert_eq!(
            pollster::block_on(host.status()),
            Ok(AutostartState::Disabled)
        );
        let state = pollster::block_on(host.enable(AutostartOptions {
            args: vec!["--tray".to_owned()],
        }));
        assert_eq!(state, Ok(AutostartState::Enabled));
        let text = std::fs::read_to_string(&entry).unwrap();
        assert!(
            text.contains("Exec=\"/opt/My Notes/notes\" --tray --autostart\n"),
            "{text}"
        );
        assert!(text.contains("Name=Notes\n"), "{text}");
        assert_eq!(
            pollster::block_on(host.status()),
            Ok(AutostartState::Enabled)
        );

        // The user turned it off (Plasma writes `Hidden=true`).
        std::fs::write(&entry, format!("{text}Hidden=true\n")).unwrap();
        assert_eq!(
            pollster::block_on(host.status()),
            Ok(AutostartState::DisabledByUser)
        );

        assert_eq!(
            pollster::block_on(host.disable()),
            Ok(AutostartState::Disabled)
        );
        assert!(!entry.exists());
        assert_eq!(
            pollster::block_on(host.disable()),
            Ok(AutostartState::Disabled)
        );
        assert_eq!(
            pollster::block_on(host.status()),
            Ok(AutostartState::Disabled)
        );
        assert_eq!(
            pollster::block_on(host.open_settings()),
            Err(AutostartError::Unsupported)
        );
    }

    #[test]
    fn enable_after_user_disable_rewrites_the_entry() {
        let dir = TempDir::new("re-enable");
        let host = host(&dir.0, "/usr/bin/notes");
        let entry = dir.0.join("autostart/com.example.notes.desktop");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(
            &entry,
            "[Desktop Entry]\nExec=/usr/bin/notes\nX-GNOME-Autostart-enabled=false\n",
        )
        .unwrap();
        assert_eq!(
            pollster::block_on(host.status()),
            Ok(AutostartState::DisabledByUser)
        );
        assert_eq!(
            pollster::block_on(host.enable(AutostartOptions::default())),
            Ok(AutostartState::Enabled)
        );
        assert_eq!(
            pollster::block_on(host.status()),
            Ok(AutostartState::Enabled)
        );
    }
}
