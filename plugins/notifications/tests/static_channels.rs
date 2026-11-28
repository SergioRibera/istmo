//! `[[app.notification_channel]]` as istmo-build encodes it must decode
//! as this crate's `Vec<ChannelSpec>` (Android reads the same bytes from
//! `res/raw/istmo_notification_channels.bin`).

use istmo_build::AppConfig;
use istmo_notifications::{
    ChannelId, ChannelSpec, Interruption, LockScreen, Sound, channels_from_build,
};

const TOML: &str = r##"
[app.notifications]
sounds = ["assets/ding.ogg"]

[[app.notification_channel]]
id = "messages"
name = "Messages"
description = "New messages"
importance = "high"
sound = "ding"
vibration = [0, 250, 100, 300]
light_color = "#E0662B"
lock_screen = "hide_content"
group = "Chats"

[[app.notification_channel]]
id = "sync:status"
name = "Sync"
importance = "passive"
sound = "silent"
show_badge = false
bypass_dnd = true

[[app.notification_channel]]
id = "alarms"
name = "Alarms"
importance = "critical"
sound = "default"
lock_screen = "public"

[[app.notification_channel]]
id = "quiet"
name = "Quiet"
importance = "urgent"
lock_screen = "hidden"
"##;

fn id(value: &str) -> ChannelId {
    ChannelId::try_from(value).unwrap()
}

#[test]
fn build_channels_decode_as_channel_specs() {
    let config = AppConfig::parse(TOML).unwrap();
    let hex = config.notifications.channels_hex().unwrap();
    let channels = channels_from_build(Some(&hex));
    assert_eq!(
        channels,
        [
            ChannelSpec {
                id: id("messages"),
                name: "Messages".into(),
                description: Some("New messages".into()),
                importance: Interruption::High,
                sound: Some(Sound::Named("ding".into())),
                vibration: Some(vec![0, 250, 100, 300]),
                light_color: Some(0xFFE0_662B),
                show_badge: true,
                lock_screen: Some(LockScreen::HideContent),
                bypass_dnd: false,
                group: Some("Chats".into()),
            },
            ChannelSpec {
                id: id("sync:status"),
                name: "Sync".into(),
                description: None,
                importance: Interruption::Passive,
                sound: Some(Sound::Silent),
                vibration: None,
                light_color: None,
                show_badge: false,
                lock_screen: None,
                bypass_dnd: true,
                group: None,
            },
            ChannelSpec {
                id: id("alarms"),
                name: "Alarms".into(),
                description: None,
                importance: Interruption::Critical,
                sound: Some(Sound::Default),
                vibration: None,
                light_color: None,
                show_badge: true,
                lock_screen: Some(LockScreen::Public),
                bypass_dnd: false,
                group: None,
            },
            ChannelSpec {
                id: id("quiet"),
                name: "Quiet".into(),
                description: None,
                importance: Interruption::Urgent,
                sound: None,
                vibration: None,
                light_color: None,
                show_badge: true,
                lock_screen: Some(LockScreen::Hidden),
                bypass_dnd: false,
                group: None,
            },
        ]
    );
    for channel in &channels {
        channel.validate().unwrap();
    }
    // The raw resource is the same bytes, unwrapped.
    assert_eq!(
        istmo_core::codec::decode_exact::<Vec<ChannelSpec>>(&config.notifications.channels_wire())
            .unwrap(),
        channels
    );
}

#[test]
fn the_macro_reads_the_crate_env() {
    // This crate is not an app: istmo-build sets no channel env for it.
    assert_eq!(istmo_notifications::static_channels!(), Vec::new());
}
