use std::fs;
use std::path::{Path, PathBuf};

use image::{Rgba, RgbaImage};

use super::*;
use crate::app_config::AppConfig;

fn parse(src: &str) -> Result<AppNotifications, ManifestError> {
    AppConfig::parse(src).map(|config| config.notifications)
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("istmo-notif-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

const FULL: &str = r##"
[app.notifications]
small_icon = "assets/notif.xml"
accent = "#E0662B"
sounds = ["assets/ding.caf", "assets/ding.ogg", "assets/pop.wav"]
exact_alarms = "use"
full_screen = true
live_updates = true
apple = { time_sensitive = true, communication = true, critical = false }
web_service_worker = false

[[app.notification_channel]]
id = "messages"
name = "Messages"
description = "New messages\nfrom people"
importance = "high"
sound = "ding"
vibration = [0, 250, 100, 250]
light_color = "#8000FF00"
lock_screen = "hide_content"
group = "Chats"

[[app.notification_channel]]
id = "sync:status"
name = "Sync"
importance = "passive"
sound = "silent"
show_badge = false
bypass_dnd = true
"##;

#[test]
fn absent_tables_are_empty() {
    let notifications = parse("[app]\n").unwrap();
    assert!(notifications.is_empty());
    assert_eq!(notifications.android_permissions(), Vec::new());
    assert_eq!(notifications.channels_hex(), None);
    assert!(notifications.info_plist().is_empty());
    assert!(notifications.entitlements().is_empty());
}

#[test]
fn parses_every_key() {
    let notifications = parse(FULL).unwrap();
    let config = notifications.config.as_ref().unwrap();
    assert_eq!(
        config.small_icon,
        Some(IconLayer::Vector(PathBuf::from("assets/notif.xml")))
    );
    assert_eq!(config.accent, Some(0xFFE0_662B));
    assert_eq!(config.sounds.len(), 3);
    assert_eq!(config.exact_alarms, ExactAlarms::Use);
    assert!(config.full_screen && config.live_updates && !config.web_service_worker);
    assert!(config.apple.time_sensitive && config.apple.communication && !config.apple.critical);
    let [messages, sync] = notifications.channels.as_slice() else {
        panic!("two channels");
    };
    assert_eq!(
        *messages,
        StaticChannel {
            id: "messages".into(),
            name: "Messages".into(),
            description: Some("New messages\nfrom people".into()),
            importance: Importance::High,
            sound: Some(ChannelSound::Named("ding".into())),
            vibration: Some(vec![0, 250, 100, 250]),
            light_color: Some(0x8000_FF00),
            show_badge: true,
            lock_screen: Some(ChannelLockScreen::HideContent),
            bypass_dnd: false,
            group: Some("Chats".into()),
        }
    );
    assert_eq!(sync.importance, Importance::Passive);
    assert_eq!(sync.sound, Some(ChannelSound::Silent));
    assert!(!sync.show_badge && sync.bypass_dnd);
}

#[test]
fn defaults() {
    let notifications =
        parse("[app.notifications]\n[[app.notification_channel]]\nid = \"a\"\nname = \"A\"\n")
            .unwrap();
    assert_eq!(notifications.config, Some(NotificationsConfig::default()));
    assert!(NotificationsConfig::default().web_service_worker);
    let channel = &notifications.channels[0];
    assert_eq!(channel.importance, Importance::Normal);
    assert!(channel.show_badge && !channel.bypass_dnd);
    assert_eq!(channel.sound, None);
}

#[test]
fn channels_alone_are_accepted_as_a_single_table() {
    let notifications = parse("[app.notification_channel]\nid = \"a\"\nname = \"A\"\n").unwrap();
    assert!(notifications.config.is_none());
    assert_eq!(notifications.channels.len(), 1);
    // The base permissions come with any notification table.
    assert_eq!(notifications.android_permissions().len(), 2);
}

/// Pinned: the plugin's `tests/static_channels.rs` decodes the same
/// bytes as `Vec<ChannelSpec>`.
#[test]
fn channel_wire_bytes() {
    let notifications = parse(
        "[[app.notification_channel]]\nid = \"a\"\nname = \"B\"\nimportance = \"urgent\"\n\
         vibration = [300]\nlight_color = \"#FF000001\"\n",
    )
    .unwrap();
    assert_eq!(
        notifications.channels_wire(),
        [
            1, // one channel
            1, b'a', // id
            1, b'B', // name
            0,    // description: None
            3,    // importance: Urgent
            0,    // sound: None
            1, 1, 251, 0x2C, 0x01, // vibration: Some([300])
            1, 252, 0x01, 0, 0, 0xFF, // light_color: Some(0xFF000001)
            1,    // show_badge
            0,    // lock_screen: None
            0,    // bypass_dnd
            0,    // group: None
        ]
    );
    assert_eq!(
        notifications.channels_hex().as_deref(),
        Some("01016101420003000101FB2C0101FC010000FF01000000")
    );
}

#[test]
fn permissions_follow_the_opt_ins() {
    let names = |src: &str| -> Vec<(String, Option<u32>)> {
        parse(src)
            .unwrap()
            .android_permissions()
            .into_iter()
            .map(|p| (p.name, p.max_sdk_version))
            .collect()
    };
    let base = [
        ("android.permission.POST_NOTIFICATIONS".to_owned(), None),
        ("android.permission.RECEIVE_BOOT_COMPLETED".to_owned(), None),
    ];
    assert_eq!(names("[app.notifications]\n"), base);
    let schedule = names("[app.notifications]\nexact_alarms = \"schedule\"\n");
    assert_eq!(
        schedule[2],
        ("android.permission.SCHEDULE_EXACT_ALARM".to_owned(), None)
    );
    let full = names(FULL);
    assert_eq!(
        full[2..],
        [
            ("android.permission.USE_EXACT_ALARM".to_owned(), None),
            (
                "android.permission.SCHEDULE_EXACT_ALARM".to_owned(),
                Some(32)
            ),
            ("android.permission.USE_FULL_SCREEN_INTENT".to_owned(), None),
            (
                "android.permission.POST_PROMOTED_NOTIFICATIONS".to_owned(),
                None
            ),
        ]
    );
}

#[test]
fn rejects_bad_values() {
    for (src, key) in [
        (
            "[app.notifications]\nsmall_icon = \"a.svg\"\n",
            "small_icon",
        ),
        ("[app.notifications]\naccent = \"red\"\n", "accent"),
        ("[app.notifications]\naccent = \"#12345\"\n", "accent"),
        ("[app.notifications]\nsounds = [\"a.flac\"]\n", "sounds"),
        ("[app.notifications]\nsounds = [\"Ding.wav\"]\n", "sounds"),
        ("[app.notifications]\nsounds = [\"1ding.wav\"]\n", "sounds"),
        (
            "[app.notifications]\nsounds = [\"a/x.wav\", \"b/x.ogg\"]\n",
            "sounds",
        ),
        (
            "[app.notifications]\nsounds = [\"x.wav\", \"x.caf\"]\n",
            "sounds",
        ),
        (
            "[app.notifications]\nexact_alarms = \"always\"\n",
            "exact_alarms",
        ),
    ] {
        let err = parse(src).unwrap_err();
        assert!(
            matches!(&err, ManifestError::InvalidValue { key: k, .. } if k.ends_with(key)),
            "{src}: {err}"
        );
    }
    let channel = |body: &str| parse(&format!("[[app.notification_channel]]\n{body}"));
    for (body, key) in [
        ("id = \"has space\"\nname = \"A\"\n", "id"),
        (
            &format!("id = \"{}\"\nname = \"A\"\n", "x".repeat(65)),
            "id",
        ),
        ("id = \"a\"\nname = \" \"\n", "name"),
        (
            &format!("id = \"a\"\nname = \"{}\"\n", "n".repeat(257)),
            "name",
        ),
        (
            "id = \"a\"\nname = \"A\"\ndescription = \"a\\u0007\"\n",
            "description",
        ),
        (
            "id = \"a\"\nname = \"A\"\nimportance = \"max\"\n",
            "importance",
        ),
        ("id = \"a\"\nname = \"A\"\nsound = \"ding\"\n", "sound"),
        (
            "id = \"a\"\nname = \"A\"\nlock_screen = \"secret\"\n",
            "lock_screen",
        ),
        (
            "id = \"a\"\nname = \"A\"\nlight_color = \"#GG0000\"\n",
            "light_color",
        ),
        ("id = \"a\"\nname = \"A\"\ngroup = \"\"\n", "group"),
        (
            &format!(
                "id = \"a\"\nname = \"A\"\nvibration = [{}]\n",
                ["1"; 33].join(",")
            ),
            "vibration",
        ),
    ] {
        let err = channel(body).unwrap_err();
        assert!(
            matches!(&err, ManifestError::InvalidValue { key: k, .. } if k.ends_with(key)),
            "{body}: {err}"
        );
    }
    assert!(matches!(
        channel("id = \"a\"\nname = \"A\"\nvibration = [-1]\n"),
        Err(ManifestError::TypeMismatch { .. })
    ));
    assert!(matches!(
        channel("name = \"A\"\n"),
        Err(ManifestError::Missing { .. })
    ));
    let dup = parse(
        "[[app.notification_channel]]\nid = \"a\"\nname = \"A\"\n\
         [[app.notification_channel]]\nid = \"a\"\nname = \"B\"\n",
    );
    assert!(matches!(dup, Err(ManifestError::InvalidValue { .. })));
}

#[test]
fn rejects_unknown_keys() {
    for (src, key) in [
        (
            "[app.notifications]\nbadge = true\n",
            "app.notifications.badge",
        ),
        (
            "[app.notifications]\napple = { focus = true }\n",
            "app.notifications.apple.focus",
        ),
        (
            "[[app.notification_channel]]\nid = \"a\"\nname = \"A\"\nrank = 1\n",
            "app.notification_channel.rank",
        ),
    ] {
        let err = parse(src).unwrap_err();
        assert!(
            matches!(&err, ManifestError::UnknownKey { key: k } if k == key),
            "{src}: {err}"
        );
    }
}

#[test]
fn info_plist_and_entitlements() {
    let notifications = parse(FULL).unwrap();
    let info = notifications.info_plist();
    let ours = info[IOS_INFO_KEY].as_dictionary().unwrap();
    assert_eq!(ours["TimeSensitive"].as_boolean(), Some(true));
    assert_eq!(ours["Communication"].as_boolean(), Some(true));
    assert_eq!(ours["Critical"].as_boolean(), Some(false));
    let sounds = ours["Sounds"].as_dictionary().unwrap();
    // Apple plays the .caf and the .wav, not the .ogg.
    assert_eq!(
        sounds["ding"].as_string(),
        Some("istmo_notification_sound_ding.caf")
    );
    assert_eq!(
        sounds["pop"].as_string(),
        Some("istmo_notification_sound_pop.wav")
    );
    let types: Vec<&str> = info["NSUserActivityTypes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(PlistValue::as_string)
        .collect();
    assert_eq!(types, ["INSendMessageIntent", "INStartCallIntent"]);
    let entitlements = notifications.entitlements();
    let keys: Vec<&String> = entitlements.keys().collect();
    assert_eq!(
        keys,
        [
            "com.apple.developer.usernotifications.time-sensitive",
            "com.apple.developer.usernotifications.communication",
        ]
    );
    let critical = parse("[app.notifications]\napple = { critical = true }\n").unwrap();
    assert!(
        critical
            .entitlements()
            .contains_key("com.apple.developer.usernotifications.critical-alerts")
    );
    assert!(!critical.info_plist().contains_key("NSUserActivityTypes"));
}

#[test]
fn android_values() {
    let xml = parse(FULL).unwrap().render_android_values().unwrap();
    assert!(xml.contains("<color name=\"istmo_notification_accent\">#FFE0662B</color>"));
    assert!(xml.contains("<bool name=\"istmo_notifications_full_screen\">true</bool>"));
    assert!(xml.contains("<bool name=\"istmo_notifications_live_updates\">true</bool>"));
    assert!(xml.contains(
        "<string name=\"istmo_notifications_exact_alarms\" translatable=\"false\">use</string>"
    ));
    let plain = parse("[app.notifications]\n")
        .unwrap()
        .render_android_values()
        .unwrap();
    assert!(!plain.contains("istmo_notification_accent"));
    assert!(plain.contains(">none</string>"));
}

fn write_sources(root: &Path) {
    fs::create_dir_all(root.join("assets")).unwrap();
    fs::write(root.join("assets/notif.xml"), b"<vector/>").unwrap();
    for sound in ["ding.caf", "ding.ogg", "pop.wav"] {
        fs::write(root.join("assets").join(sound), sound.as_bytes()).unwrap();
    }
}

#[test]
fn writes_and_cleans_android_resources() {
    let root = temp_dir("android");
    write_sources(&root);
    let res = root.join("res");
    let notifications = parse(FULL).unwrap().rooted(&root);
    assert_eq!(notifications.source_files().count(), 4);
    notifications.write_android(&res, None).unwrap();
    assert!(res.join("values/istmo_notifications.xml").is_file());
    assert_eq!(
        fs::read(res.join("drawable/istmo_notification_icon.xml")).unwrap(),
        b"<vector/>"
    );
    assert_eq!(
        fs::read(res.join("raw/istmo_notification_channels.bin")).unwrap(),
        notifications.channels_wire()
    );
    assert!(res.join("raw/istmo_notification_sound_ding.ogg").is_file());
    assert!(res.join("raw/istmo_notification_sound_pop.wav").is_file());
    // Android cannot play .caf.
    assert!(!res.join("raw/istmo_notification_sound_ding.caf").exists());
    // Another resource of the directory is left alone; a stale one of
    // ours goes.
    fs::write(res.join("raw/app_own.ogg"), b"x").unwrap();
    fs::write(res.join("raw/istmo_notification_sound_old.ogg"), b"x").unwrap();
    notifications.write_android(&res, None).unwrap();
    assert!(res.join("raw/app_own.ogg").exists());
    assert!(!res.join("raw/istmo_notification_sound_old.ogg").exists());
    AppNotifications::default()
        .write_android(&res, None)
        .unwrap();
    assert!(!res.join("values/istmo_notifications.xml").exists());
    assert!(!res.join("drawable/istmo_notification_icon.xml").exists());
    assert!(!res.join("raw/istmo_notification_channels.bin").exists());
    assert!(res.join("raw/app_own.ogg").exists());
    let _ = fs::remove_dir_all(&root);
}

fn png(image: &RgbaImage, path: &Path) {
    image.save(path).unwrap();
}

#[test]
fn png_small_icon_becomes_a_white_mask_per_density() {
    let root = temp_dir("png");
    let mut image = RgbaImage::from_pixel(48, 48, Rgba([0, 0, 0, 0]));
    for x in 12..36 {
        for y in 12..36 {
            image.put_pixel(x, y, Rgba([200, 30, 30, 255]));
        }
    }
    png(&image, &root.join("icon.png"));
    let notifications = AppNotifications {
        config: Some(NotificationsConfig {
            small_icon: Some(IconLayer::Png(root.join("icon.png"))),
            ..NotificationsConfig::default()
        }),
        channels: Vec::new(),
    };
    let res = root.join("res");
    notifications.write_android(&res, None).unwrap();
    for (density, edge) in SMALL_ICON_DENSITIES {
        let out = image::open(res.join(format!("drawable-{density}/istmo_notification_icon.png")))
            .unwrap()
            .to_rgba8();
        assert_eq!(out.dimensions(), (*edge, *edge));
        let centre = out.get_pixel(edge / 2, edge / 2).0;
        assert_eq!(centre, [255, 255, 255, 255]);
        assert_eq!(out.get_pixel(0, 0).0[3], 0);
    }
    // Switching to a vector removes the PNGs.
    fs::write(root.join("icon.xml"), b"<vector/>").unwrap();
    let vector = AppNotifications {
        config: Some(NotificationsConfig {
            small_icon: Some(IconLayer::Vector(root.join("icon.xml"))),
            ..NotificationsConfig::default()
        }),
        channels: Vec::new(),
    };
    vector.write_android(&res, None).unwrap();
    assert!(
        !res.join("drawable-mdpi/istmo_notification_icon.png")
            .exists()
    );
    assert!(res.join("drawable/istmo_notification_icon.xml").is_file());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn opaque_png_and_missing_sources_fail_as_input_errors() {
    let root = temp_dir("opaque");
    png(
        &RgbaImage::from_pixel(8, 8, Rgba([0, 0, 0, 255])),
        &root.join("icon.png"),
    );
    let opaque = AppNotifications {
        config: Some(NotificationsConfig {
            small_icon: Some(IconLayer::Png(root.join("icon.png"))),
            ..NotificationsConfig::default()
        }),
        channels: Vec::new(),
    };
    let err = opaque.write_android(&root.join("res"), None).unwrap_err();
    assert!(matches!(err, NotificationsError::OpaqueIcon { .. }) && !err.is_write());
    let missing = parse(FULL).unwrap().rooted(Path::new("/nonexistent-istmo"));
    let err = missing.write_android(&root.join("res"), None).unwrap_err();
    assert!(matches!(err, NotificationsError::Read { .. }) && !err.is_write());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn monochrome_layer_stands_in_for_the_small_icon() {
    let root = temp_dir("mono");
    // A 108 dp layer: only the central 72 dp are visible.
    let mut layer = RgbaImage::from_pixel(108, 108, Rgba([0, 0, 0, 0]));
    for x in 40..68 {
        for y in 40..68 {
            layer.put_pixel(x, y, Rgba([0, 0, 0, 255]));
        }
    }
    png(&layer, &root.join("mono.png"));
    let res = root.join("res");
    parse("[app.notifications]\n")
        .unwrap()
        .write_android(&res, Some(&IconLayer::Png(root.join("mono.png"))))
        .unwrap();
    let out = image::open(res.join("drawable-xxxhdpi/istmo_notification_icon.png"))
        .unwrap()
        .to_rgba8();
    assert_eq!(out.get_pixel(48, 48).0, [255, 255, 255, 255]);
    // An explicit small icon wins.
    fs::write(root.join("icon.xml"), b"<vector/>").unwrap();
    parse(&format!(
        "[app.notifications]\nsmall_icon = \"{}\"\n",
        root.join("icon.xml").display()
    ))
    .unwrap()
    .write_android(&res, Some(&IconLayer::Png(root.join("mono.png"))))
    .unwrap();
    assert!(res.join("drawable/istmo_notification_icon.xml").is_file());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn stages_ios_sounds() {
    let root = temp_dir("ios");
    write_sources(&root);
    let dir = root.join("ios/.istmo/NotificationSounds");
    let notifications = parse(FULL).unwrap().rooted(&root);
    assert_eq!(
        notifications.write_ios_sounds(&dir).unwrap(),
        Some(dir.clone())
    );
    let mut files: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            "istmo_notification_sound_ding.caf",
            "istmo_notification_sound_pop.wav"
        ]
    );
    assert_eq!(
        AppNotifications::default().write_ios_sounds(&dir).unwrap(),
        None
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn desktop_env_lines() {
    let root = temp_dir("desktop");
    write_sources(&root);
    fs::write(root.join("sw.js"), b"// sw").unwrap();
    let out = root.join("out");
    let mut notifications = parse(FULL).unwrap().rooted(&root);
    let lines = notifications.desktop_env_lines(&out, None).unwrap();
    let sound_dir = out.join("istmo-notification-sounds");
    assert_eq!(
        lines,
        [
            format!(
                "cargo::rustc-env=ISTMO_NOTIFICATION_CHANNELS={}",
                notifications.channels_hex().unwrap()
            ),
            "cargo::rustc-env=ISTMO_NOTIFICATION_ACCENT=FFE0662B".to_owned(),
            "cargo::rustc-env=ISTMO_NOTIFICATION_APPLE=time_sensitive,communication".to_owned(),
            format!(
                "cargo::rustc-env=ISTMO_NOTIFICATION_SOUND_DIR={}",
                sound_dir.display()
            ),
        ]
    );
    // Every sound, whatever the platform plays.
    assert_eq!(fs::read_dir(&sound_dir).unwrap().count(), 3);
    assert!(sound_dir.join("ding.ogg").is_file());
    // `web_service_worker = false` stages nothing.
    let none = notifications
        .desktop_env_lines(&out, Some(&root.join("sw.js")))
        .unwrap();
    assert!(!none.iter().any(|l| l.contains(ENV_SERVICE_WORKER)));
    if let Some(config) = notifications.config.as_mut() {
        config.web_service_worker = true;
    }
    let with_worker = notifications
        .desktop_env_lines(&out, Some(&root.join("sw.js")))
        .unwrap();
    let staged = out
        .join("istmo-notifications-web")
        .join(SERVICE_WORKER_FILE);
    assert_eq!(
        with_worker.last().unwrap(),
        &format!("cargo::rustc-env={ENV_SERVICE_WORKER}={}", staged.display())
    );
    assert_eq!(fs::read(&staged).unwrap(), b"// sw");
    let _ = fs::remove_dir_all(&root);
}
