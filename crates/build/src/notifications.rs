//! `[app.notifications]` and `[[app.notification_channel]]` — what the
//! `istmo-notifications` plugin needs from the app at build time.
//!
//! ```toml
//! [app.notifications]
//! small_icon   = "assets/notif.xml"   # Android: vector .xml or a transparent .png
//! accent       = "#E0662B"
//! sounds       = ["assets/ding.wav"]  # Sound::Named("ding")
//! exact_alarms = "none"               # "schedule" | "use"
//! full_screen  = false
//! live_updates = false
//! apple = { time_sensitive = false, communication = false, critical = false }
//! web_service_worker = true
//!
//! [[app.notification_channel]]
//! id = "messages"
//! name = "Messages"
//! importance = "high"
//! sound = "ding"
//! ```
//!
//! Every value is checked here, with the limits of the plugin's
//! `ChannelSpec::validate` (keep them in step), so a bad table fails the
//! build instead of the first `show` on a device. What each platform
//! gets is listed in `design/notifications.md`, "Implementation notes
//! (istmo-build, as built)"; the names below are that contract.
//!
//! - **Android** — resources in the generated `android/.istmo/res`
//!   ([`AppNotifications::write_android`]) and permissions in the
//!   `istmo-permissions.xml` sidecar ([`AppNotifications::android_permissions`]).
//! - **Apple** — `Info.plist` and entitlement keys
//!   ([`AppNotifications::info_plist`], [`AppNotifications::entitlements`])
//!   merged into the `istmo:plugins` blocks, and the sounds staged in
//!   `ios/.istmo/NotificationSounds` with the static channels.
//! - **Desktop / web** — `cargo::rustc-env` values for the app crate
//!   ([`AppNotifications::emit_desktop`]), read by the plugin's
//!   `static_channels!()`.

use std::fmt;
use std::fmt::Write as _;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, RgbaImage};
use plist::{Dictionary, Value as PlistValue};
use toml_edit::{Item, TableLike, Value};
use wincode::config::Configuration;
use wincode::int_encoding::{LittleEndian, VarInt};
use wincode::len::BincodeLen;
use wincode::{SchemaRead, SchemaWrite};

use crate::app_icon::IconLayer;
use crate::manifest::{AndroidPermission, ManifestError, expect_bool, expect_string, expect_table};

/// Plugin id of `istmo-notifications`.
pub const PLUGIN_ID: &str = "istmo.notifications";

/// Android drawable of the small icon (`drawable/` vector or
/// `drawable-<density>/` PNGs).
pub const ANDROID_SMALL_ICON: &str = "istmo_notification_icon";
/// Android colour resource of `accent`.
pub const ANDROID_ACCENT: &str = "istmo_notification_accent";
/// Android raw resource with the static channels (wire bytes of
/// `Vec<ChannelSpec>`), file `res/raw/istmo_notification_channels.bin`.
pub const ANDROID_CHANNELS: &str = "istmo_notification_channels";
/// Prefix of the Android raw resource of a sound: `Sound::Named("ding")`
/// is `R.raw.istmo_notification_sound_ding`.
pub const ANDROID_SOUND_PREFIX: &str = "istmo_notification_sound_";
/// Android bool resource: `full_screen`.
pub const ANDROID_FULL_SCREEN: &str = "istmo_notifications_full_screen";
/// Android bool resource: `live_updates`.
pub const ANDROID_LIVE_UPDATES: &str = "istmo_notifications_live_updates";
/// Android string resource: `exact_alarms` (`none` / `schedule` / `use`).
pub const ANDROID_EXACT_ALARMS: &str = "istmo_notifications_exact_alarms";
/// `values/` file holding the colour, bool and string resources.
const ANDROID_VALUES_FILE: &str = "istmo_notifications.xml";

/// `Info.plist` dictionary the Swift backend reads.
pub const IOS_INFO_KEY: &str = "IstmoNotifications";
/// Staged sound directory, relative to the iOS project root; its files
/// are bundle resources (copied flat into the bundle root).
pub const IOS_SOUNDS_DIR: &str = ".istmo/NotificationSounds";
/// Bundle resource holding the static channels (wire `Vec<ChannelSpec>`),
/// `<bundle root>/istmo_notification_channels.bin`, staged in
/// [`IOS_SOUNDS_DIR`].
pub const IOS_CHANNELS: &str = ANDROID_CHANNELS;

/// Hex wire bytes of the static channels (`Vec<ChannelSpec>`).
pub const ENV_CHANNELS: &str = "ISTMO_NOTIFICATION_CHANNELS";
/// Directory of the staged desktop sounds, `<name>.<ext>`.
pub const ENV_SOUND_DIR: &str = "ISTMO_NOTIFICATION_SOUND_DIR";
/// `accent` as 8 hex digits, `AARRGGBB`.
pub const ENV_ACCENT: &str = "ISTMO_NOTIFICATION_ACCENT";
/// Comma-separated set `apple` flags (`time_sensitive`, `communication`,
/// `critical`), for the macOS backend.
pub const ENV_APPLE: &str = "ISTMO_NOTIFICATION_APPLE";
/// Path of the staged service worker (wasm32 targets).
pub const ENV_SERVICE_WORKER: &str = "ISTMO_NOTIFICATION_SERVICE_WORKER";
/// Service worker file the plugin ships as an `[[assets]]` entry.
pub const SERVICE_WORKER_FILE: &str = "istmo-notifications-sw.js";

const KNOWN_KEYS: &[&str] = &[
    "small_icon",
    "accent",
    "sounds",
    "exact_alarms",
    "full_screen",
    "live_updates",
    "apple",
    "web_service_worker",
];
const KNOWN_APPLE_KEYS: &[&str] = &["time_sensitive", "communication", "critical"];
const KNOWN_CHANNEL_KEYS: &[&str] = &[
    "id",
    "name",
    "description",
    "importance",
    "sound",
    "vibration",
    "light_color",
    "show_badge",
    "lock_screen",
    "bypass_dnd",
    "group",
];

/// The plugin's limits (`ids.rs`, `validate.rs`).
const MAX_ID_LEN: usize = 64;
const MAX_LABEL_CHARS: usize = 128;
const MAX_BODY_CHARS: usize = 4096;
const MAX_VIBRATION_STEPS: usize = 32;
/// Android resource names stay well below aapt's limits with the prefix.
const MAX_SOUND_NAME_LEN: usize = 64;

/// Small icon edge per density bucket (24 dp).
const SMALL_ICON_DENSITIES: &[(&str, u32)] = &[
    ("mdpi", 24),
    ("hdpi", 36),
    ("xhdpi", 48),
    ("xxhdpi", 72),
    ("xxxhdpi", 96),
];

/// Same layout as `istmo_core::codec` (bincode 2 `standard()`); the
/// plugin's `tests/static_channels.rs` decodes these bytes as
/// `Vec<ChannelSpec>`.
type WireConfig = Configuration<true, { 1 << 20 }, BincodeLen, LittleEndian, VarInt, u32>;

/// A failure while writing the notification outputs.
#[derive(Debug)]
pub enum NotificationsError {
    /// A source file (icon, sound, service worker) could not be read.
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The small icon PNG could not be decoded or encoded.
    Image {
        path: PathBuf,
        source: image::ImageError,
    },
    /// The small icon PNG has no transparent pixel: Android would draw a
    /// filled square.
    OpaqueIcon { path: PathBuf },
    /// A generated file could not be written.
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl NotificationsError {
    /// Whether this is a failed write into a generated directory (only
    /// warns, per the crate's error policy) rather than bad input.
    #[must_use]
    pub const fn is_write(&self) -> bool {
        matches!(self, Self::Write { .. })
    }
}

impl fmt::Display for NotificationsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "reading {}: {source}", path.display()),
            Self::Image { path, source } => {
                write!(
                    f,
                    "`[app.notifications] small_icon` {}: {source}",
                    path.display()
                )
            }
            Self::OpaqueIcon { path } => write!(
                f,
                "`[app.notifications] small_icon` {} has no transparent pixel; Android draws \
                 the small icon from its alpha channel only, so it would be a filled square",
                path.display()
            ),
            Self::Write { path, source } => write!(f, "writing {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for NotificationsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Write { source, .. } => Some(source),
            Self::Image { source, .. } => Some(source),
            Self::OpaqueIcon { .. } => None,
        }
    }
}

/// `exact_alarms`: which exact-alarm permission the app declares.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ExactAlarms {
    /// Inexact alarms only.
    #[default]
    None,
    /// `SCHEDULE_EXACT_ALARM` (user-grantable, denied by default on 14+).
    Schedule,
    /// `USE_EXACT_ALARM` (33+, alarm / calendar apps under Play policy),
    /// plus `SCHEDULE_EXACT_ALARM` up to API 32.
    Use,
}

impl ExactAlarms {
    const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Schedule => "schedule",
            Self::Use => "use",
        }
    }
}

/// `apple`: Apple capabilities the app has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppleNotifications {
    /// `com.apple.developer.usernotifications.time-sensitive`.
    pub time_sensitive: bool,
    /// `com.apple.developer.usernotifications.communication` and
    /// `NSUserActivityTypes` (`INSendMessageIntent`, `INStartCallIntent`).
    pub communication: bool,
    /// `com.apple.developer.usernotifications.critical-alerts` (Apple
    /// grants it on request).
    pub critical: bool,
}

/// Container of a bundled sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundFormat {
    Wav,
    Ogg,
    Mp3,
    Caf,
    Aiff,
}

impl SoundFormat {
    fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "wav" => Some(Self::Wav),
            "ogg" => Some(Self::Ogg),
            "mp3" => Some(Self::Mp3),
            "caf" => Some(Self::Caf),
            "aif" | "aiff" => Some(Self::Aiff),
            _ => None,
        }
    }

    /// Played by Android channels and `NotificationCompat`.
    #[must_use]
    pub const fn android(self) -> bool {
        matches!(self, Self::Wav | Self::Ogg | Self::Mp3)
    }

    /// Played by `UNNotificationSound`.
    #[must_use]
    pub const fn apple(self) -> bool {
        matches!(self, Self::Wav | Self::Caf | Self::Aiff)
    }
}

/// One `sounds` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoundFile {
    /// File stem: the `Sound::Named` value, `[a-z][a-z0-9_]*`.
    pub name: String,
    pub path: PathBuf,
    pub format: SoundFormat,
}

impl SoundFile {
    fn parse(raw: &str) -> Result<Self, ManifestError> {
        let invalid = |expected| ManifestError::InvalidValue {
            key: "app.notifications.sounds".to_owned(),
            value: raw.to_owned(),
            expected,
        };
        let path = PathBuf::from(raw);
        let format = path
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(SoundFormat::from_extension)
            .ok_or_else(|| invalid("a .wav, .ogg, .mp3, .caf, .aif or .aiff file"))?;
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| is_resource_name(stem))
            .ok_or_else(|| {
                invalid(
                    "a file name of 1..=64 characters of [a-z0-9_] starting with a letter \
                     (it is the sound's name, and an Android resource name)",
                )
            })?
            .to_owned();
        Ok(Self { name, path, format })
    }

    fn file_name(&self) -> String {
        let ext = self
            .path
            .extension()
            .and_then(|ext| ext.to_str())
            .map_or_else(String::new, str::to_ascii_lowercase);
        format!("{}.{ext}", self.name)
    }

    fn read(&self) -> Result<Vec<u8>, NotificationsError> {
        fs::read(&self.path).map_err(|source| NotificationsError::Read {
            path: self.path.clone(),
            source,
        })
    }
}

/// `[app.notifications]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationsConfig {
    /// Android small icon; `None` falls back to
    /// `[app.icon.android] monochrome`.
    pub small_icon: Option<IconLayer>,
    /// Accent colour, ARGB.
    pub accent: Option<u32>,
    pub sounds: Vec<SoundFile>,
    pub exact_alarms: ExactAlarms,
    /// `USE_FULL_SCREEN_INTENT`.
    pub full_screen: bool,
    /// `POST_PROMOTED_NOTIFICATIONS` (Android 16 Live Updates).
    pub live_updates: bool,
    pub apple: AppleNotifications,
    /// Stage the plugin's service worker for web builds (default on).
    pub web_service_worker: bool,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            small_icon: None,
            accent: None,
            sounds: Vec::new(),
            exact_alarms: ExactAlarms::None,
            full_screen: false,
            live_updates: false,
            apple: AppleNotifications::default(),
            web_service_worker: true,
        }
    }
}

impl NotificationsConfig {
    fn parse(item: &Item) -> Result<Self, ManifestError> {
        let table = expect_table(item, "app.notifications")?;
        reject_unknown(table, KNOWN_KEYS, "app.notifications")?;
        let string = |key: &'static str, full: &'static str| {
            table
                .get(key)
                .map(|item| expect_string(item, full))
                .transpose()
        };
        let boolean = |key: &'static str, full: &'static str| {
            table
                .get(key)
                .map(|item| expect_bool(item, full))
                .transpose()
        };
        let small_icon = string("small_icon", "app.notifications.small_icon")?
            .map(|raw| {
                let path = PathBuf::from(raw);
                match extension(&path).as_deref() {
                    Some("png") => Ok(IconLayer::Png(path)),
                    Some("xml") => Ok(IconLayer::Vector(path)),
                    _ => Err(ManifestError::InvalidValue {
                        key: "app.notifications.small_icon".to_owned(),
                        value: raw.to_owned(),
                        expected: "an Android vector drawable `.xml` or a transparent `.png`",
                    }),
                }
            })
            .transpose()?;
        let accent = string("accent", "app.notifications.accent")?
            .map(|raw| parse_argb(raw, "app.notifications.accent"))
            .transpose()?;
        let sounds = table
            .get("sounds")
            .map(parse_sounds)
            .transpose()?
            .unwrap_or_default();
        let exact_alarms = match string("exact_alarms", "app.notifications.exact_alarms")? {
            None | Some("none") => ExactAlarms::None,
            Some("schedule") => ExactAlarms::Schedule,
            Some("use") => ExactAlarms::Use,
            Some(other) => {
                return Err(ManifestError::InvalidValue {
                    key: "app.notifications.exact_alarms".to_owned(),
                    value: other.to_owned(),
                    expected: "`none`, `schedule` or `use`",
                });
            }
        };
        let apple = table
            .get("apple")
            .map(parse_apple)
            .transpose()?
            .unwrap_or_default();
        Ok(Self {
            small_icon,
            accent,
            sounds,
            exact_alarms,
            full_screen: boolean("full_screen", "app.notifications.full_screen")?.unwrap_or(false),
            live_updates: boolean("live_updates", "app.notifications.live_updates")?
                .unwrap_or(false),
            apple,
            web_service_worker: boolean(
                "web_service_worker",
                "app.notifications.web_service_worker",
            )?
            .unwrap_or(true),
        })
    }
}

fn parse_sounds(item: &Item) -> Result<Vec<SoundFile>, ManifestError> {
    let mismatch = || ManifestError::TypeMismatch {
        key: "app.notifications.sounds".to_owned(),
        expected: "array of strings",
    };
    let array = item.as_array().ok_or_else(mismatch)?;
    let mut sounds: Vec<SoundFile> = Vec::with_capacity(array.len());
    for value in array {
        let Value::String(raw) = value else {
            return Err(mismatch());
        };
        let sound = SoundFile::parse(raw.value())?;
        // One file per name and platform family: `ding.caf` + `ding.ogg`
        // covers Apple and Android, `ding.wav` + `ding.ogg` does not.
        let clash = sounds.iter().any(|other| {
            other.name == sound.name
                && (other.path == sound.path
                    || (other.format.android() && sound.format.android())
                    || (other.format.apple() && sound.format.apple()))
        });
        if clash {
            return Err(ManifestError::InvalidValue {
                key: "app.notifications.sounds".to_owned(),
                value: raw.value().clone(),
                expected: "one file per sound name for Android (.wav/.ogg/.mp3) and one for \
                           Apple (.wav/.caf/.aiff)",
            });
        }
        sounds.push(sound);
    }
    Ok(sounds)
}

fn parse_apple(item: &Item) -> Result<AppleNotifications, ManifestError> {
    let table = expect_table(item, "app.notifications.apple")?;
    reject_unknown(table, KNOWN_APPLE_KEYS, "app.notifications.apple")?;
    let flag = |key: &'static str, full: &'static str| {
        table
            .get(key)
            .map(|item| expect_bool(item, full))
            .transpose()
            .map(Option::unwrap_or_default)
    };
    Ok(AppleNotifications {
        time_sensitive: flag("time_sensitive", "app.notifications.apple.time_sensitive")?,
        communication: flag("communication", "app.notifications.apple.communication")?,
        critical: flag("critical", "app.notifications.apple.critical")?,
    })
}

/// Default interruption of a channel; same variant order as the
/// plugin's `Interruption` (the wire tag).
#[derive(Debug, Clone, Copy, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub enum Importance {
    Passive,
    Normal,
    High,
    Urgent,
    Critical,
}

impl Importance {
    fn parse(raw: Option<&str>) -> Result<Self, ManifestError> {
        match raw {
            None | Some("normal") => Ok(Self::Normal),
            Some("passive") => Ok(Self::Passive),
            Some("high") => Ok(Self::High),
            Some("urgent") => Ok(Self::Urgent),
            Some("critical") => Ok(Self::Critical),
            Some(other) => Err(ManifestError::InvalidValue {
                key: "app.notification_channel.importance".to_owned(),
                value: other.to_owned(),
                expected: "`passive`, `normal`, `high`, `urgent` or `critical`",
            }),
        }
    }
}

/// Channel sound; same variant order as the plugin's `Sound`.
#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub enum ChannelSound {
    Default,
    Silent,
    /// A name from `[app.notifications] sounds`.
    Named(String),
}

/// Lock-screen visibility; same variant order as the plugin's `LockScreen`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub enum ChannelLockScreen {
    Public,
    HideContent,
    Hidden,
}

impl ChannelLockScreen {
    fn parse(raw: &str) -> Result<Self, ManifestError> {
        match raw {
            "public" => Ok(Self::Public),
            "hide_content" => Ok(Self::HideContent),
            "hidden" => Ok(Self::Hidden),
            other => Err(ManifestError::InvalidValue {
                key: "app.notification_channel.lock_screen".to_owned(),
                value: other.to_owned(),
                expected: "`public`, `hide_content` or `hidden`",
            }),
        }
    }
}

/// One `[[app.notification_channel]]`, field for field the plugin's
/// `ChannelSpec` (`id` is the `ChannelId { value }` newtype there, which
/// encodes the same).
#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub struct StaticChannel {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub importance: Importance,
    pub sound: Option<ChannelSound>,
    pub vibration: Option<Vec<u32>>,
    pub light_color: Option<u32>,
    pub show_badge: bool,
    pub lock_screen: Option<ChannelLockScreen>,
    pub bypass_dnd: bool,
    pub group: Option<String>,
}

impl StaticChannel {
    fn parse(table: &dyn TableLike, sounds: &[SoundFile]) -> Result<Self, ManifestError> {
        reject_unknown(table, KNOWN_CHANNEL_KEYS, "app.notification_channel")?;
        let invalid =
            |key: &str, value: &str, expected: &'static str| ManifestError::InvalidValue {
                key: format!("app.notification_channel.{key}"),
                value: value.to_owned(),
                expected,
            };
        let string = |key: &'static str, full: &'static str| {
            table
                .get(key)
                .map(|item| expect_string(item, full))
                .transpose()
        };
        let boolean = |key: &'static str, full: &'static str| {
            table
                .get(key)
                .map(|item| expect_bool(item, full))
                .transpose()
        };
        let id = string("id", "app.notification_channel.id")?.ok_or(ManifestError::Missing {
            key: "app.notification_channel.id",
        })?;
        if !is_valid_id(id) {
            return Err(invalid("id", id, "1..=64 bytes of [A-Za-z0-9._:-]"));
        }
        let name =
            string("name", "app.notification_channel.name")?.ok_or(ManifestError::Missing {
                key: "app.notification_channel.name",
            })?;
        if name.trim().is_empty() || !is_plain_text(name, MAX_LABEL_CHARS * 2, false) {
            return Err(invalid(
                "name",
                name,
                "a non-blank name of at most 256 characters, no control characters",
            ));
        }
        let description = string("description", "app.notification_channel.description")?;
        if let Some(text) = description
            && !is_plain_text(text, MAX_BODY_CHARS, true)
        {
            return Err(invalid(
                "description",
                text,
                "at most 4096 characters, no control characters but newlines and tabs",
            ));
        }
        let importance =
            Importance::parse(string("importance", "app.notification_channel.importance")?)?;
        let sound = string("sound", "app.notification_channel.sound")?
            .map(|raw| match raw {
                "default" => Ok(ChannelSound::Default),
                "silent" => Ok(ChannelSound::Silent),
                name if sounds.iter().any(|s| s.name == name) => {
                    Ok(ChannelSound::Named(name.to_owned()))
                }
                other => Err(invalid(
                    "sound",
                    other,
                    "`default`, `silent` or the name of a file in `[app.notifications] sounds`",
                )),
            })
            .transpose()?;
        let vibration = table.get("vibration").map(parse_vibration).transpose()?;
        let light_color = string("light_color", "app.notification_channel.light_color")?
            .map(|raw| parse_argb(raw, "app.notification_channel.light_color"))
            .transpose()?;
        let lock_screen = string("lock_screen", "app.notification_channel.lock_screen")?
            .map(ChannelLockScreen::parse)
            .transpose()?;
        let group = string("group", "app.notification_channel.group")?;
        if let Some(group) = group
            && (group.trim().is_empty() || !is_plain_text(group, MAX_LABEL_CHARS, false))
        {
            return Err(invalid(
                "group",
                group,
                "a non-blank group name of at most 128 characters, no control characters",
            ));
        }
        Ok(Self {
            id: id.to_owned(),
            name: name.to_owned(),
            description: description.map(str::to_owned),
            importance,
            sound,
            vibration,
            light_color,
            show_badge: boolean("show_badge", "app.notification_channel.show_badge")?
                .unwrap_or(true),
            lock_screen,
            bypass_dnd: boolean("bypass_dnd", "app.notification_channel.bypass_dnd")?
                .unwrap_or(false),
            group: group.map(str::to_owned),
        })
    }
}

fn parse_vibration(item: &Item) -> Result<Vec<u32>, ManifestError> {
    let mismatch = || ManifestError::TypeMismatch {
        key: "app.notification_channel.vibration".to_owned(),
        expected: "array of milliseconds (integers 0..=4294967295)",
    };
    let array = item.as_array().ok_or_else(mismatch)?;
    if array.len() > MAX_VIBRATION_STEPS {
        return Err(ManifestError::InvalidValue {
            key: "app.notification_channel.vibration".to_owned(),
            value: format!("{} steps", array.len()),
            expected: "at most 32 steps",
        });
    }
    array
        .iter()
        .map(|value| {
            value
                .as_integer()
                .and_then(|ms| u32::try_from(ms).ok())
                .ok_or_else(mismatch)
        })
        .collect()
}

/// `[app.notifications]` plus every `[[app.notification_channel]]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppNotifications {
    /// `None` when the app has no `[app.notifications]` table.
    pub config: Option<NotificationsConfig>,
    /// Declaration order.
    pub channels: Vec<StaticChannel>,
}

impl AppNotifications {
    /// Parse `app.notifications` and `app.notification_channel` (one
    /// table or an array of tables).
    ///
    /// # Errors
    ///
    /// Unknown keys, values of the wrong type, anything the plugin's
    /// validation refuses, duplicate channel ids and channel sounds not
    /// listed in `sounds`.
    pub fn parse(config: Option<&Item>, channels: Option<&Item>) -> Result<Self, ManifestError> {
        let config = config.map(NotificationsConfig::parse).transpose()?;
        let sounds = config.as_ref().map_or(&[][..], |c| c.sounds.as_slice());
        let channels: Vec<StaticChannel> = match channels {
            None => Vec::new(),
            Some(item) => {
                if let Some(array) = item.as_array_of_tables() {
                    array
                        .iter()
                        .map(|table| StaticChannel::parse(table, sounds))
                        .collect::<Result<_, _>>()?
                } else {
                    vec![StaticChannel::parse(
                        expect_table(item, "app.notification_channel")?,
                        sounds,
                    )?]
                }
            }
        };
        for (index, channel) in channels.iter().enumerate() {
            if channels[..index].iter().any(|c| c.id == channel.id) {
                return Err(ManifestError::InvalidValue {
                    key: "app.notification_channel.id".to_owned(),
                    value: channel.id.clone(),
                    expected: "each channel id declared once",
                });
            }
        }
        Ok(Self { config, channels })
    }

    /// Whether the app declares nothing for notifications.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.config.is_none() && self.channels.is_empty()
    }

    /// These settings with file paths anchored at `root` (the crate).
    #[must_use]
    pub fn rooted(&self, root: &Path) -> Self {
        let config = self.config.as_ref().map(|config| NotificationsConfig {
            small_icon: config.small_icon.as_ref().map(|icon| match icon {
                IconLayer::Png(path) => IconLayer::Png(root.join(path)),
                IconLayer::Vector(path) => IconLayer::Vector(root.join(path)),
            }),
            sounds: config
                .sounds
                .iter()
                .map(|sound| SoundFile {
                    path: root.join(&sound.path),
                    ..sound.clone()
                })
                .collect(),
            ..config.clone()
        });
        Self {
            config,
            channels: self.channels.clone(),
        }
    }

    fn sounds(&self) -> &[SoundFile] {
        self.config.as_ref().map_or(&[], |c| c.sounds.as_slice())
    }

    /// Files the outputs are made from (icon and sounds).
    pub fn source_files(&self) -> impl Iterator<Item = &Path> {
        let icon = self
            .config
            .as_ref()
            .and_then(|c| c.small_icon.as_ref())
            .map(IconLayer::path);
        icon.into_iter()
            .chain(self.sounds().iter().map(|s| s.path.as_path()))
    }

    /// Wire bytes of the static channels: the plugin's `Vec<ChannelSpec>`.
    ///
    /// # Panics
    ///
    /// Never for parsed channels (every field is bounded).
    #[must_use]
    pub fn channels_wire(&self) -> Vec<u8> {
        wincode::config::serialize(&self.channels, WireConfig::new())
            .expect("invariant: bounded channel fields always encode")
    }

    /// [`Self::channels_wire`] as upper-case hex, `None` without channels.
    #[must_use]
    pub fn channels_hex(&self) -> Option<String> {
        if self.channels.is_empty() {
            return None;
        }
        let bytes = self.channels_wire();
        let mut hex = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(hex, "{byte:02X}");
        }
        Some(hex)
    }

    /// Permissions for the `istmo-permissions.xml` sidecar: the plugin's
    /// base set and the opt-ins of `[app.notifications]`.
    #[must_use]
    pub fn android_permissions(&self) -> Vec<AndroidPermission> {
        if self.is_empty() {
            return Vec::new();
        }
        let mut out = vec![
            AndroidPermission::new("android.permission.POST_NOTIFICATIONS"),
            AndroidPermission::new("android.permission.RECEIVE_BOOT_COMPLETED"),
        ];
        let Some(config) = &self.config else {
            return out;
        };
        match config.exact_alarms {
            ExactAlarms::None => {}
            ExactAlarms::Schedule => out.push(AndroidPermission::new(
                "android.permission.SCHEDULE_EXACT_ALARM",
            )),
            ExactAlarms::Use => {
                out.push(AndroidPermission::new("android.permission.USE_EXACT_ALARM"));
                // `USE_EXACT_ALARM` exists from API 33; 31 and 32 need this one.
                out.push(AndroidPermission::with_max_sdk(
                    "android.permission.SCHEDULE_EXACT_ALARM",
                    32,
                ));
            }
        }
        if config.full_screen {
            out.push(AndroidPermission::new(
                "android.permission.USE_FULL_SCREEN_INTENT",
            ));
        }
        if config.live_updates {
            out.push(AndroidPermission::new(
                "android.permission.POST_PROMOTED_NOTIFICATIONS",
            ));
        }
        out
    }

    /// `values/istmo_notifications.xml`, or `None` without
    /// `[app.notifications]`.
    #[must_use]
    pub fn render_android_values(&self) -> Option<String> {
        let config = self.config.as_ref()?;
        let mut out = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
             <!-- GENERATED by istmo-build from [app.notifications] - DO NOT EDIT. -->\n\
             <resources>\n",
        );
        if let Some(argb) = config.accent {
            let _ = writeln!(
                out,
                "    <color name=\"{ANDROID_ACCENT}\">#{argb:08X}</color>"
            );
        }
        let _ = writeln!(
            out,
            "    <bool name=\"{ANDROID_FULL_SCREEN}\">{}</bool>\n    \
             <bool name=\"{ANDROID_LIVE_UPDATES}\">{}</bool>\n    \
             <string name=\"{ANDROID_EXACT_ALARMS}\" translatable=\"false\">{}</string>",
            config.full_screen,
            config.live_updates,
            config.exact_alarms.as_str(),
        );
        out.push_str("</resources>\n");
        Some(out)
    }

    /// Write the Android resources under `res_dir` (`android/.istmo/res`)
    /// and remove what an earlier configuration wrote and this one does
    /// not. `monochrome` (`[app.icon.android] monochrome`, absolute)
    /// stands in for a missing `small_icon`. Something is written exactly
    /// when [`Self::is_empty`] is false.
    ///
    /// # Errors
    ///
    /// An unreadable or opaque source ([`NotificationsError::is_write`]
    /// false), or a failed write.
    pub fn write_android(
        &self,
        res_dir: &Path,
        monochrome: Option<&IconLayer>,
    ) -> Result<(), NotificationsError> {
        let mut written: Vec<PathBuf> = Vec::new();
        if let Some(config) = &self.config {
            let values = res_dir.join("values").join(ANDROID_VALUES_FILE);
            if let Some(xml) = self.render_android_values() {
                write(&values, xml.as_bytes())?;
                written.push(values);
            }
            let icon = config
                .small_icon
                .as_ref()
                .map(|icon| (icon, false))
                .or_else(|| monochrome.map(|icon| (icon, true)));
            if let Some((icon, adaptive_layer)) = icon {
                written.extend(write_small_icon(res_dir, icon, adaptive_layer)?);
            }
            for sound in config.sounds.iter().filter(|s| s.format.android()) {
                let dest = res_dir
                    .join("raw")
                    .join(format!("{ANDROID_SOUND_PREFIX}{}", sound.file_name()));
                write(&dest, &sound.read()?)?;
                written.push(dest);
            }
        }
        if !self.channels.is_empty() {
            let dest = res_dir.join("raw").join(format!("{ANDROID_CHANNELS}.bin"));
            write(&dest, &self.channels_wire())?;
            written.push(dest);
        }
        remove_stale_android(res_dir, &written)
    }

    /// Keys for the app's `Info.plist` (merged into the `istmo:plugins`
    /// block). Empty without `[app.notifications]`.
    #[must_use]
    pub fn info_plist(&self) -> Dictionary {
        let mut out = Dictionary::new();
        let Some(config) = &self.config else {
            return out;
        };
        let mut ours = Dictionary::new();
        let apple = config.apple;
        ours.insert("TimeSensitive".into(), apple.time_sensitive.into());
        ours.insert("Communication".into(), apple.communication.into());
        ours.insert("Critical".into(), apple.critical.into());
        let mut sounds = Dictionary::new();
        for sound in config.sounds.iter().filter(|s| s.format.apple()) {
            sounds.insert(sound.name.clone(), ios_sound_file(sound).into());
        }
        ours.insert("Sounds".into(), PlistValue::Dictionary(sounds));
        out.insert(IOS_INFO_KEY.into(), PlistValue::Dictionary(ours));
        if apple.communication {
            out.insert(
                "NSUserActivityTypes".into(),
                PlistValue::Array(vec![
                    "INSendMessageIntent".into(),
                    "INStartCallIntent".into(),
                ]),
            );
        }
        out
    }

    /// Entitlements for the app (merged into the `istmo:plugins` block of
    /// its `.entitlements`). Also what macOS packaging must sign with.
    #[must_use]
    pub fn entitlements(&self) -> Dictionary {
        let mut out = Dictionary::new();
        let Some(apple) = self.config.as_ref().map(|c| c.apple) else {
            return out;
        };
        for (set, key) in [
            (
                apple.time_sensitive,
                "com.apple.developer.usernotifications.time-sensitive",
            ),
            (
                apple.communication,
                "com.apple.developer.usernotifications.communication",
            ),
            (
                apple.critical,
                "com.apple.developer.usernotifications.critical-alerts",
            ),
        ] {
            if set {
                out.insert(key.into(), true.into());
            }
        }
        out
    }

    /// Stage the iOS bundle resources into `dir`
    /// (`ios/.istmo/NotificationSounds`, copied flat into the bundle root):
    /// the Apple-playable sounds under the names [`Self::info_plist`]
    /// lists, and the static channels as `istmo_notification_channels.bin`
    /// (wire `Vec<ChannelSpec>`, the bytes Android gets in `res/raw`).
    /// Removes stale files. Returns `dir` when it holds anything.
    ///
    /// # Errors
    ///
    /// An unreadable sound or a failed write.
    pub fn write_ios_resources(&self, dir: &Path) -> Result<Option<PathBuf>, NotificationsError> {
        let mut written = Vec::new();
        for sound in self.sounds().iter().filter(|s| s.format.apple()) {
            let dest = dir.join(ios_sound_file(sound));
            write(&dest, &sound.read()?)?;
            written.push(dest);
        }
        if !self.channels.is_empty() {
            let dest = dir.join(format!("{IOS_CHANNELS}.bin"));
            write(&dest, &self.channels_wire())?;
            written.push(dest);
        }
        remove_stale_in(dir, &written, |_| true)?;
        // `ios/.istmo` already carries the generated-dir `.gitignore`.
        Ok((!written.is_empty()).then(|| dir.to_path_buf()))
    }

    /// `cargo::rustc-env` lines for a desktop or web build of the app
    /// crate, staging sounds (and, on wasm32, the plugin's service
    /// worker `service_worker`) under `out_dir`.
    ///
    /// # Errors
    ///
    /// An unreadable sound or service worker, or a failed write.
    pub fn desktop_env_lines(
        &self,
        out_dir: &Path,
        service_worker: Option<&Path>,
    ) -> Result<Vec<String>, NotificationsError> {
        let mut lines = Vec::new();
        if let Some(hex) = self.channels_hex() {
            lines.push(format!("cargo::rustc-env={ENV_CHANNELS}={hex}"));
        }
        let Some(config) = &self.config else {
            return Ok(lines);
        };
        if let Some(argb) = config.accent {
            lines.push(format!("cargo::rustc-env={ENV_ACCENT}={argb:08X}"));
        }
        let apple: Vec<&str> = [
            (config.apple.time_sensitive, "time_sensitive"),
            (config.apple.communication, "communication"),
            (config.apple.critical, "critical"),
        ]
        .into_iter()
        .filter_map(|(set, name)| set.then_some(name))
        .collect();
        if !apple.is_empty() {
            lines.push(format!("cargo::rustc-env={ENV_APPLE}={}", apple.join(",")));
        }
        let sound_dir = out_dir.join("istmo-notification-sounds");
        let mut written = Vec::new();
        for sound in &config.sounds {
            let dest = sound_dir.join(sound.file_name());
            write(&dest, &sound.read()?)?;
            written.push(dest);
        }
        remove_stale_in(&sound_dir, &written, |_| true)?;
        if !written.is_empty() {
            lines.push(format!(
                "cargo::rustc-env={ENV_SOUND_DIR}={}",
                sound_dir.display()
            ));
        }
        if let Some(source) = service_worker.filter(|_| config.web_service_worker) {
            let bytes = fs::read(source).map_err(|err| NotificationsError::Read {
                path: source.to_path_buf(),
                source: err,
            })?;
            let dest = out_dir
                .join("istmo-notifications-web")
                .join(SERVICE_WORKER_FILE);
            write(&dest, &bytes)?;
            lines.push(format!(
                "cargo::rustc-env={ENV_SERVICE_WORKER}={}",
                dest.display()
            ));
        }
        Ok(lines)
    }
}

/// Bundle file of an Apple sound (prefixed: the files land in the
/// bundle root next to the app's own resources).
fn ios_sound_file(sound: &SoundFile) -> String {
    format!("{ANDROID_SOUND_PREFIX}{}", sound.file_name())
}

/// Write the small icon; `adaptive_layer`: the source is a 108 dp
/// adaptive layer whose visible part is its central 72 dp.
fn write_small_icon(
    res_dir: &Path,
    icon: &IconLayer,
    adaptive_layer: bool,
) -> Result<Vec<PathBuf>, NotificationsError> {
    match icon {
        IconLayer::Vector(path) => {
            let bytes = fs::read(path).map_err(|source| NotificationsError::Read {
                path: path.clone(),
                source,
            })?;
            let dest = res_dir
                .join("drawable")
                .join(format!("{ANDROID_SMALL_ICON}.xml"));
            write(&dest, &bytes)?;
            Ok(vec![dest])
        }
        IconLayer::Png(path) => {
            let mask = alpha_mask(path, adaptive_layer)?;
            let mut written = Vec::with_capacity(SMALL_ICON_DENSITIES.len());
            for (density, edge) in SMALL_ICON_DENSITIES {
                let resized = image::imageops::resize(&mask, *edge, *edge, FilterType::Lanczos3);
                let mut bytes = Vec::new();
                DynamicImage::ImageRgba8(resized)
                    .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
                    .map_err(|source| NotificationsError::Image {
                        path: path.clone(),
                        source,
                    })?;
                let dest = res_dir
                    .join(format!("drawable-{density}"))
                    .join(format!("{ANDROID_SMALL_ICON}.png"));
                write(&dest, &bytes)?;
                written.push(dest);
            }
            Ok(written)
        }
    }
}

/// White pixels carrying the source's alpha, square-cropped (the central
/// two thirds of an adaptive layer).
fn alpha_mask(path: &Path, adaptive_layer: bool) -> Result<RgbaImage, NotificationsError> {
    let bytes = fs::read(path).map_err(|source| NotificationsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut image = image::load_from_memory(&bytes)
        .map_err(|source| NotificationsError::Image {
            path: path.to_path_buf(),
            source,
        })?
        .to_rgba8();
    if adaptive_layer {
        let (w, h) = image.dimensions();
        image = image::imageops::crop_imm(&image, w / 6, h / 6, w * 2 / 3, h * 2 / 3).to_image();
    }
    if image.pixels().all(|pixel| pixel.0[3] == u8::MAX) {
        return Err(NotificationsError::OpaqueIcon {
            path: path.to_path_buf(),
        });
    }
    for pixel in image.pixels_mut() {
        let alpha = pixel.0[3];
        pixel.0 = [u8::MAX, u8::MAX, u8::MAX, alpha];
    }
    Ok(image)
}

/// Remove `istmo_notification*` files under `res_dir` that `written`
/// does not list.
fn remove_stale_android(res_dir: &Path, written: &[PathBuf]) -> Result<(), NotificationsError> {
    let Ok(dirs) = fs::read_dir(res_dir) else {
        return Ok(());
    };
    for dir in dirs.filter_map(Result::ok) {
        let ours = |path: &Path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("istmo_notification"))
        };
        remove_stale_in(&dir.path(), written, ours)?;
    }
    Ok(())
}

/// Remove the files of `dir` matching `ours` that `written` does not list.
fn remove_stale_in(
    dir: &Path,
    written: &[PathBuf],
    ours: impl Fn(&Path) -> bool,
) -> Result<(), NotificationsError> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_file() && ours(&path) && !written.contains(&path) && !is_marker(&path) {
            fs::remove_file(&path).map_err(|source| NotificationsError::Write { path, source })?;
        }
    }
    Ok(())
}

/// The `.gitignore` [`crate::fsx::mark_generated_dir`] writes.
fn is_marker(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == ".gitignore")
}

fn write(dest: &Path, contents: &[u8]) -> Result<(), NotificationsError> {
    crate::fsx::write_if_changed(dest, contents)
        .map(drop)
        .map_err(|source| NotificationsError::Write {
            path: dest.to_path_buf(),
            source,
        })
}

fn reject_unknown(
    table: &dyn TableLike,
    known: &[&str],
    prefix: &str,
) -> Result<(), ManifestError> {
    for (name, _) in table.iter() {
        if !known.contains(&name) {
            return Err(ManifestError::UnknownKey {
                key: format!("{prefix}.{name}"),
            });
        }
    }
    Ok(())
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
}

/// `#RRGGBB` (opaque) or `#AARRGGBB`.
fn parse_argb(raw: &str, key: &str) -> Result<u32, ManifestError> {
    let digits = raw.strip_prefix('#').unwrap_or_default();
    let value = (matches!(digits.len(), 6 | 8) && digits.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(digits, 16).ok())
        .flatten();
    match value {
        Some(rgb) if digits.len() == 6 => Ok(0xFF00_0000 | rgb),
        Some(argb) => Ok(argb),
        None => Err(ManifestError::InvalidValue {
            key: key.to_owned(),
            value: raw.to_owned(),
            expected: "`#RRGGBB` or `#AARRGGBB`",
        }),
    }
}

/// The plugin's id rule: `1..=64` bytes of `[A-Za-z0-9._:-]`.
fn is_valid_id(id: &str) -> bool {
    (1..=MAX_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

/// At most `max` characters, no control characters (newlines and tabs
/// pass when `multiline`).
fn is_plain_text(value: &str, max: usize, multiline: bool) -> bool {
    value.chars().count() <= max
        && value
            .chars()
            .all(|c| !c.is_control() || (multiline && matches!(c, '\n' | '\t')))
}

/// `[a-z][a-z0-9_]*`, the Android resource rule.
fn is_resource_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= MAX_SOUND_NAME_LEN
        && bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
mod tests;
