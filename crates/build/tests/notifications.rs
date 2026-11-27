//! `[app.notifications]` + `[[app.notification_channel]]`: build
//! failures, golden outputs and the app pass end to end.

use std::fs;
use std::path::{Path, PathBuf};

use istmo_build::{AppConfig, AppOpts, ManifestError, PlistFragments, emit_app_with};

const TOML: &str = r##"[app]
id = "com.example.chat"

[app.notifications]
small_icon = "assets/notif.xml"
accent = "#E0662B"
sounds = ["assets/ding.caf", "assets/ding.ogg"]
exact_alarms = "schedule"
full_screen = true
apple = { time_sensitive = true, communication = true }

[[app.notification_channel]]
id = "messages"
name = "Messages"
importance = "high"
sound = "ding"
"##;

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn assert_golden(name: &str, actual: &str) {
    let path = golden_dir().join(format!("{name}.expected"));
    if std::env::var("ISTMO_BUILD_UPDATE_GOLDEN").is_ok() {
        fs::write(&path, actual).expect("update golden");
        return;
    }
    let expected =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert_eq!(
        actual, expected,
        "golden mismatch for {name}\nRun `ISTMO_BUILD_UPDATE_GOLDEN=1 cargo test -p istmo-build` to refresh."
    );
}

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("istmo-notif-it-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(path.join("assets")).unwrap();
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("src/lib.rs"), "fn main() {}\n").unwrap();
    fs::write(path.join("istmo.toml"), TOML).unwrap();
    fs::write(path.join("assets/notif.xml"), "<vector/>").unwrap();
    fs::write(path.join("assets/ding.caf"), "caf").unwrap();
    fs::write(path.join("assets/ding.ogg"), "ogg").unwrap();
    path
}

fn plist_body(dict: plist::Dictionary) -> String {
    let mut fragments = PlistFragments::default();
    fragments.add("app", dict);
    fragments.render_body().unwrap()
}

#[test]
fn goldens() {
    let notifications = AppConfig::parse(TOML).unwrap().notifications;
    assert_golden(
        "notifications_values.xml",
        &notifications.render_android_values().unwrap(),
    );
    assert_golden(
        "notifications_info_plist.xml",
        &plist_body(notifications.info_plist()),
    );
    assert_golden(
        "notifications_entitlements.xml",
        &plist_body(notifications.entitlements()),
    );
}

#[test]
fn bad_tables_fail_with_the_key() {
    for (src, message) in [
        (
            "[app.notifications]\nsound = \"x\"\n",
            "istmo.toml unknown key `app.notifications.sound`",
        ),
        (
            "[[app.notification_channel]]\nid = \"a\"\nname = \"A\"\nsound = \"ding\"\n",
            "istmo.toml key `app.notification_channel.sound` = `ding`",
        ),
        (
            "[app.notifications]\nexact_alarms = true\n",
            "istmo.toml key `app.notifications.exact_alarms` expected string",
        ),
        (
            "[[app.notification_channel]]\nname = \"A\"\n",
            "istmo.toml missing required key `app.notification_channel.id`",
        ),
    ] {
        let err: ManifestError = AppConfig::parse(src).unwrap_err();
        assert!(err.to_string().starts_with(message), "{src}: {err}");
    }
}

#[test]
fn android_permissions_land_in_the_sidecar() {
    let root = scratch("android");
    let android_root = root.join("android");
    fs::create_dir_all(android_root.join("app/src/main/java")).unwrap();
    fs::write(
        android_root.join("app/build.gradle.kts"),
        "android {\n    namespace = \"com.example.chat\"\n}\n",
    )
    .unwrap();
    emit_app_with(AppOpts {
        root: Some(root.clone()),
        android_root: Some(android_root.clone()),
        ios: Some(false),
        lib_name: Some("app".into()),
        ..AppOpts::default()
    });
    let sidecar = fs::read_to_string(android_root.join(".istmo/istmo-permissions.xml")).unwrap();
    for permission in [
        "POST_NOTIFICATIONS",
        "RECEIVE_BOOT_COMPLETED",
        "SCHEDULE_EXACT_ALARM",
        "USE_FULL_SCREEN_INTENT",
    ] {
        assert!(
            sidecar.contains(&format!("android.permission.{permission}\"")),
            "{permission}:\n{sidecar}"
        );
    }
    assert!(!sidecar.contains("POST_PROMOTED_NOTIFICATIONS"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ios_keys_entitlements_and_sounds() {
    let root = scratch("ios");
    let ios_root = root.join("ios");
    let app = ios_root.join("Chat");
    fs::create_dir_all(&app).unwrap();
    let marked = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
                  <!-- istmo:plugins:start -->\n<!-- istmo:plugins:end -->\n</dict>\n</plist>\n";
    fs::write(app.join("Info.plist"), marked).unwrap();
    fs::write(app.join("Chat.entitlements"), marked).unwrap();
    let opts = || AppOpts {
        root: Some(root.clone()),
        ios_root: Some(ios_root.clone()),
        android: Some(false),
        lib_name: Some("app".into()),
        ..AppOpts::default()
    };
    emit_app_with(opts());
    let info = fs::read_to_string(app.join("Info.plist")).unwrap();
    assert!(info.contains("<key>IstmoNotifications</key>"), "{info}");
    assert!(info.contains("<string>istmo_notification_sound_ding.caf</string>"));
    assert!(info.contains("<string>INSendMessageIntent</string>"));
    let entitlements = fs::read_to_string(app.join("Chat.entitlements")).unwrap();
    assert!(entitlements.contains("com.apple.developer.usernotifications.time-sensitive"));
    assert!(entitlements.contains("com.apple.developer.usernotifications.communication"));
    assert!(!entitlements.contains("critical-alerts"));
    let sounds = ios_root.join(".istmo/NotificationSounds");
    assert_eq!(
        fs::read(sounds.join("istmo_notification_sound_ding.caf")).unwrap(),
        b"caf"
    );
    assert!(!sounds.join("istmo_notification_sound_ding.ogg").exists());
    // The static channels ship in the bundle root next to the sounds.
    let channels = fs::read(sounds.join("istmo_notification_channels.bin")).unwrap();
    assert!(!channels.is_empty());
    let fragment = fs::read_to_string(app.join("istmo-plugins.yml")).unwrap();
    assert!(fragment.contains("NotificationSounds"), "{fragment}");

    // Dropping the table clears the keys and the sounds again.
    fs::write(
        root.join("istmo.toml"),
        "[app]\nid = \"com.example.chat\"\n",
    )
    .unwrap();
    emit_app_with(opts());
    let info = fs::read_to_string(app.join("Info.plist")).unwrap();
    assert!(!info.contains("IstmoNotifications"), "{info}");
    let entitlements = fs::read_to_string(app.join("Chat.entitlements")).unwrap();
    assert!(!entitlements.contains("usernotifications"));
    assert!(!sounds.join("istmo_notification_sound_ding.caf").exists());
    assert!(!sounds.join("istmo_notification_channels.bin").exists());
    let fragment = fs::read_to_string(app.join("istmo-plugins.yml")).unwrap();
    assert!(!fragment.contains("NotificationSounds"));
    let _ = fs::remove_dir_all(&root);
}
