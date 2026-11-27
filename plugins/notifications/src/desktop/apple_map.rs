//! Rules of the macOS backend that do not touch `UserNotifications`:
//! what it reports, how a notification maps to request fields, how a
//! response maps back. Compiled on macOS and in tests everywhere, so the
//! mapping is unit-tested on Linux (like screen-capture's `sck_map.rs`).
//!
//! Raw values are the SDK's: `UNNotificationPresentationOptions`,
//! `UNNotificationActionOptions`, `UNNotificationCategoryOptions`,
//! `UNAuthorizationOptions`, `UNNotificationInterruptionLevel`,
//! `UNAuthorizationStatus`, `UNNotificationSetting`.

#![cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "used by the macOS backend; elsewhere compiled only for its tests"
    )
)]

use std::collections::HashSet;
use std::path::Path;

use crate::coalesce::Rate;
use crate::render::{Input, RenderLimits, Rendered, RenderedAction};
use crate::{
    Capabilities, Feature, FeatureSupport, Interruption, LockScreen, Notification,
    PermissionRequest, PermissionState, PermissionStatus, Sound, Style, Support,
};

use super::backend::{BackendInfo, ImageFiles, ImageSlot};

/// Actions macOS shows (in the banner's "Options" menu).
pub(super) const MAX_ACTIONS: u32 = 10;

/// `userInfo` keys (the same as the iOS backend's).
pub(super) mod keys {
    /// The notification id.
    pub(in crate::desktop) const ID: &str = "istmo.id";
    /// `Notification::user_data`.
    pub(in crate::desktop) const USER_DATA: &str = "istmo.ud";
    /// `Notification::url`.
    pub(in crate::desktop) const URL: &str = "istmo.url";
    /// Foreground presentation, raw `UNNotificationPresentationOptions`.
    pub(in crate::desktop) const PRESENT: &str = "istmo.p";
}

/// `UNNotificationPresentationOptions` bits.
mod present {
    pub(super) const BADGE: u64 = 1 << 0;
    pub(super) const SOUND: u64 = 1 << 1;
    /// `UNNotificationPresentationOptionAlert`: the only visible option
    /// before macOS 11 (deprecated there, replaced by `LIST | BANNER`).
    pub(super) const ALERT: u64 = 1 << 2;
    pub(super) const LIST: u64 = 1 << 3;
    pub(super) const BANNER: u64 = 1 << 4;
}

/// Entitlement flags of the running bundle: the `IstmoNotifications`
/// dictionary of its `Info.plist` (the one istmo-build writes for iOS;
/// macOS packaging writes the same keys). Each interruption level needs
/// its entitlement; without it a level falls back one step, as on iOS.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Entitlements {
    /// `TimeSensitive`: `com.apple.developer.usernotifications.time-sensitive`.
    pub(super) time_sensitive: bool,
    /// `Critical`: `com.apple.developer.usernotifications.critical-alerts`.
    pub(super) critical: bool,
}

/// The `Info.plist` key holding [`Entitlements`].
pub(super) const INFO_KEY: &str = "IstmoNotifications";

/// What the backend reports. `communication` = communication
/// notifications are set up (macOS 12+, `INSendMessageIntent` in
/// `NSUserActivityTypes`).
pub(super) fn info(os_version: &str, communication: bool) -> BackendInfo {
    use Feature::{
        Actions, Avatar, BackgroundAction, Chronometer, CustomSound, Dismissal, DoNotDisturb,
        ExactAlarms, Groups, InlineReply, Interruption as InterruptionLevel, ListDelivered,
        LivePromotion, LockScreen as LockScreenPrivacy, Messaging, Ongoing, OpenSettings,
        PermissionRequest as Permission, Picture, Show, SilentUpdate, TapWhenClosed, Update,
    };
    let rich = if communication {
        Support::Native
    } else {
        Support::Simulated
    };
    let mut features = Vec::new();
    let mut add = |support: Support, list: &[Feature]| {
        features.extend(
            list.iter()
                .map(|&feature| FeatureSupport { feature, support }),
        );
    };
    add(
        Support::Native,
        &[
            Show,
            Update,
            Actions,
            InlineReply,
            Picture,
            Groups,
            CustomSound,
            InterruptionLevel,
            ListDelivered,
            Dismissal,
            TapWhenClosed,
            BackgroundAction,
            Permission,
            OpenSettings,
        ],
    );
    add(rich, &[Messaging, Avatar]);
    add(Support::Simulated, &[SilentUpdate, LockScreenPrivacy]);
    add(
        Support::Unsupported,
        &[
            LivePromotion,
            Ongoing,
            Chronometer,
            ExactAlarms,
            DoNotDisturb,
        ],
    );
    let capabilities = Capabilities {
        backend: format!("macos:{os_version}"),
        features,
        max_actions: MAX_ACTIONS,
        max_pending: None,
        max_active: None,
    };
    let mut limits = RenderLimits::from_capabilities(&capabilities);
    limits.subtitle = true;
    BackendInfo {
        capabilities,
        limits,
        rate: Rate::APPLE,
        lists_delivered: true,
        timeouts: false,
    }
}

/// `UNUserNotificationCenter` works only in a bundled app with a bundle
/// id: unbundled, `currentNotificationCenter` throws inside the
/// framework.
pub(super) fn is_bundled(bundle_path: &str, bundle_id: Option<&str>) -> bool {
    bundle_id.is_some_and(|id| !id.is_empty())
        && Path::new(bundle_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
}

/// Foreground presentation of `n`. A silent update only refreshes the
/// Notification Center entry.
pub(super) fn presentation(n: &Notification, quiet: bool) -> u64 {
    if n.hide_in_foreground {
        return 0;
    }
    if quiet {
        return present::LIST;
    }
    let Some(fg) = n.apple.as_ref().and_then(|apple| apple.foreground) else {
        return present::BANNER | present::LIST | present::SOUND;
    };
    let bit = |on: bool, bit: u64| if on { bit } else { 0 };
    bit(fg.banner, present::BANNER)
        | bit(fg.list, present::LIST)
        | bit(fg.sound, present::SOUND)
        | bit(fg.badge, present::BADGE)
}

/// What `willPresent` answers for a notification without our key (an
/// app's own) when no earlier delegate handles it.
pub(super) const DEFAULT_PRESENTATION: u64 = present::BANNER | present::LIST | present::SOUND;

/// The options `willPresent` hands the system for stored bits:
/// unchanged on macOS 11+; on 10.15 `BANNER` becomes `ALERT` and a
/// list-only presentation (a silent update) becomes nothing, since 10.15
/// has no list-only option.
pub(super) const fn presentation_options(bits: u64, big_sur: bool) -> u64 {
    if big_sur {
        return bits;
    }
    let kept = bits & (present::BADGE | present::SOUND);
    if bits & present::BANNER != 0 {
        kept | present::ALERT
    } else {
        kept
    }
}

/// Raw `UNNotificationInterruptionLevel`.
pub(super) const fn interruption_level(
    level: Option<Interruption>,
    quiet: bool,
    entitlements: Entitlements,
) -> usize {
    const ACTIVE: usize = 1;
    const TIME_SENSITIVE: usize = 2;
    const CRITICAL: usize = 3;
    if quiet {
        return 0;
    }
    let urgent = if entitlements.time_sensitive {
        TIME_SENSITIVE
    } else {
        ACTIVE
    };
    match level {
        Some(Interruption::Passive) => 0,
        None | Some(Interruption::Normal | Interruption::High) => ACTIVE,
        Some(Interruption::Critical) if entitlements.critical => CRITICAL,
        Some(Interruption::Urgent | Interruption::Critical) => urgent,
    }
}

/// The sound of a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SoundChoice {
    None,
    Default,
    Critical,
    /// A bundled sound file name (with extension when known).
    Named(String),
}

/// `quiet` = a silent update. The critical sound needs the
/// critical-alerts entitlement; without it the notification's own sound
/// plays.
pub(super) fn sound(n: &Notification, quiet: bool, entitlements: Entitlements) -> SoundChoice {
    if quiet {
        return SoundChoice::None;
    }
    if n.interruption == Some(Interruption::Critical) && entitlements.critical {
        return SoundChoice::Critical;
    }
    match &n.sound {
        None | Some(Sound::Default) => SoundChoice::Default,
        Some(Sound::Silent) => SoundChoice::None,
        Some(Sound::Named(name)) => SoundChoice::Named(name.clone()),
    }
}

/// Sound file extensions tried in the bundle for `Sound::Named`.
pub(super) const SOUND_EXTENSIONS: [&str; 4] = ["caf", "wav", "aiff", "aif"];

/// Thread identifier: the group, else the conversation.
pub(super) fn thread_id(n: &Notification) -> Option<&str> {
    match (&n.group, &n.style) {
        (Some(group), _) => Some(group.id.as_str()),
        (None, Style::Messaging(_)) => Some(n.id.as_str()),
        _ => None,
    }
}

/// The image attached to a request: the picture, else the icon.
pub(super) fn attachment(images: &ImageFiles) -> Option<&Path> {
    images
        .get(&ImageSlot::Picture)
        .or_else(|| images.get(&ImageSlot::Icon))
}

/// File extension of an image from its first bytes (PNG, JPEG, GIF):
/// `UNNotificationAttachment` infers the type from the extension, so a
/// file without one gets it from its content. `None` when unknown.
pub(super) fn image_extension(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if head.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("gif")
    } else {
        None
    }
}

/// Raw `UNNotificationActionOptions` of a button.
pub(super) const fn action_options(action: &RenderedAction) -> usize {
    let mut bits = 0;
    if action.requires_unlock {
        bits |= 1 << 0;
    }
    if action.destructive {
        bits |= 1 << 1;
    }
    if action.opens_app {
        bits |= 1 << 2;
    }
    bits
}

/// Raw `UNNotificationCategoryOptions` (custom dismiss always, to hear
/// about dismissals) and the hidden-preview placeholder.
pub(super) const fn category_options(lock: Option<LockScreen>) -> (usize, Option<&'static str>) {
    const CUSTOM_DISMISS: usize = 1 << 0;
    const SHOW_TITLE: usize = 1 << 2;
    const SHOW_SUBTITLE: usize = 1 << 3;
    match lock {
        None => (CUSTOM_DISMISS, None),
        Some(LockScreen::Public) => (CUSTOM_DISMISS | SHOW_TITLE | SHOW_SUBTITLE, None),
        Some(LockScreen::HideContent | LockScreen::Hidden) => (CUSTOM_DISMISS, Some("")),
    }
}

/// Category identifier of a rendered notification: a content hash of the
/// buttons (reply placeholder and submit title included) and options, so
/// equal sets share one category.
pub(super) fn category_id(
    n: &Notification,
    rendered: &Rendered,
    lock: Option<LockScreen>,
    communication: bool,
) -> String {
    let mut hash = Fnv::default();
    hash.write(&[
        lock.map_or(0xff, |lock| lock as u8),
        u8::from(communication),
    ]);
    for action in &rendered.actions {
        hash.write(action.key.as_bytes());
        hash.write(&[0x1f]);
        hash.write(action.title.as_bytes());
        hash.write(&[
            0x1f,
            u8::from(action.opens_app),
            u8::from(action.destructive),
            u8::from(action.requires_unlock),
        ]);
        if action.input == Input::Reply {
            let (placeholder, submit) = reply_texts(n, action);
            hash.write(b"\x1freply\x1f");
            hash.write(placeholder.as_bytes());
            hash.write(&[0x1f]);
            hash.write(submit.as_bytes());
        }
    }
    format!("{CATEGORY_PREFIX}{:016x}", hash.0)
}

/// Prefix of this plugin's category identifiers.
pub(super) const CATEGORY_PREFIX: &str = "istmo.c.";

/// Which of `ours` (oldest first) to unregister so at most `max` stay:
/// the oldest ones no delivered or pending notification uses (`in_use`),
/// never `keep` (the one being registered). Fewer than needed (even none)
/// when the rest are in use: a category in use outlives the cap.
pub(super) fn evict<'a>(
    ours: &[&'a str],
    max: usize,
    keep: &str,
    in_use: &HashSet<String>,
) -> Vec<&'a str> {
    let mut extra = ours.len().saturating_sub(max);
    let mut out = Vec::with_capacity(extra);
    for &id in ours {
        if extra == 0 {
            break;
        }
        if id != keep && !in_use.contains(id) {
            out.push(id);
            extra -= 1;
        }
    }
    out
}

/// FNV-1a 64.
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// Text input fields of a reply action: placeholder and submit title.
pub(super) fn reply_texts(n: &Notification, action: &RenderedAction) -> (String, String) {
    let options = n.actions.iter().find_map(|a| match &a.kind {
        crate::ActionKind::Reply(options) if a.id.as_str() == action.key => Some(options),
        _ => None,
    });
    (
        options
            .and_then(|o| o.placeholder.clone())
            .unwrap_or_default(),
        options
            .and_then(|o| o.submit_title.clone())
            .unwrap_or_else(|| "Send".to_owned()),
    )
}

/// What a `UNNotificationResponse` action identifier means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResponseKind {
    /// `UNNotificationDismissActionIdentifier`.
    Dismissed,
    /// `UNNotificationDefaultActionIdentifier`: the notification itself.
    Opened,
    /// A button, by its rendered key.
    Button(String),
}

pub(super) const DEFAULT_ACTION: &str = "com.apple.UNNotificationDefaultActionIdentifier";
pub(super) const DISMISS_ACTION: &str = "com.apple.UNNotificationDismissActionIdentifier";

pub(super) fn response_kind(action_identifier: &str) -> ResponseKind {
    match action_identifier {
        DISMISS_ACTION => ResponseKind::Dismissed,
        DEFAULT_ACTION => ResponseKind::Opened,
        key => ResponseKind::Button(key.to_owned()),
    }
}

/// Raw `UNAuthorizationOptions` of a request.
pub(super) const fn authorization_options(request: PermissionRequest) -> usize {
    let mut bits = 0;
    if request.badge {
        bits |= 1 << 0;
    }
    if request.sound {
        bits |= 1 << 1;
    }
    if request.alerts {
        bits |= 1 << 2;
    }
    if request.critical {
        bits |= 1 << 4;
    }
    if request.provisional {
        bits |= 1 << 6;
    }
    bits
}

/// `UNNotificationSettings`, read as raw values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct RawSettings {
    pub(super) authorization: isize,
    pub(super) alert: isize,
    pub(super) sound: isize,
    pub(super) badge: isize,
    /// macOS 12+ only.
    pub(super) time_sensitive: Option<isize>,
    /// macOS 12+ only.
    pub(super) direct_messages: Option<isize>,
}

/// `UNNotificationSetting.enabled`.
const ENABLED: isize = 2;

/// `UNAuthorizationStatusDenied`: posts are refused with
/// `PermissionDenied` (the system would drop them silently).
pub(super) const fn denied(authorization: isize) -> bool {
    authorization == 1
}

pub(super) fn permission(raw: RawSettings) -> PermissionStatus {
    let state = match raw.authorization {
        1 => PermissionState::Denied,
        2 => PermissionState::Granted,
        3 => PermissionState::Provisional,
        4 => PermissionState::Ephemeral,
        // 0 and anything a later SDK adds.
        _ => PermissionState::NotDetermined,
    };
    PermissionStatus {
        alerts: raw.alert == ENABLED,
        sound: raw.sound == ENABLED,
        badge: raw.badge == ENABLED,
        time_sensitive: raw.time_sensitive.map(|s| s == ENABLED),
        communication: raw.direct_messages.map(|s| s == ENABLED),
        ..PermissionStatus::of(state)
    }
}

/// A launch caused by a notification: `AppKit` puts the response under
/// `NSApplicationLaunchUserNotificationKey`, or marks the launch as not
/// a default one.
pub(super) const fn launched_by_notification(
    has_notification_key: bool,
    is_default: Option<bool>,
) -> bool {
    has_notification_key || matches!(is_default, Some(false))
}

/// How long after a notification launch its response still counts as
/// the one that launched the app.
pub(super) const LAUNCH_WINDOW_MS: i64 = 10_000;

/// `launched_app` of a response at `now_ms`, given the launch mark this
/// response took (`0` = none: not a notification launch, or an earlier
/// response took it). The mark is per launch and consumed by the first
/// response; past [`LAUNCH_WINDOW_MS`] the launch had another cause (a
/// non-default launch also covers documents and URLs), so a late
/// response does not claim it.
pub(super) const fn launched_app(mark_ms: i64, now_ms: i64) -> bool {
    mark_ms > 0 && now_ms >= mark_ms && now_ms - mark_ms <= LAUNCH_WINDOW_MS
}

/// The app's page in System Settings > Notifications.
pub(super) fn settings_url(bundle_id: &str) -> String {
    format!("x-apple.systempreferences:com.apple.Notifications-Settings.extension?id={bundle_id}")
}

/// Whether a communication notification can be built for `n`: the
/// last message has a sender (an incoming one).
pub(super) fn communication_sender(n: &Notification) -> Option<&crate::Participant> {
    match &n.style {
        Style::Messaging(conversation) => conversation.messages.last()?.sender.as_ref(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::render;
    use crate::{Action, AppleExtras, ChatMessage, Conversation, ForegroundOptions, Participant};

    fn n() -> Notification {
        Notification::new("n", "Title").unwrap()
    }

    #[test]
    fn bundle_check() {
        assert!(is_bundled("/Applications/Demo.app", Some("dev.istmo.demo")));
        assert!(is_bundled("/x/Demo.APP", Some("a.b")));
        assert!(!is_bundled("/usr/local/bin", Some("a.b")));
        assert!(!is_bundled("/Applications/Demo.app", None));
        assert!(!is_bundled("/Applications/Demo.app", Some("")));
    }

    #[test]
    fn presentation_bits() {
        assert_eq!(presentation(&n(), false), 0b1_1010);
        assert_eq!(presentation(&n(), true), present::LIST);
        let mut hidden = n();
        hidden.hide_in_foreground = true;
        assert_eq!(presentation(&hidden, false), 0);
        let mut custom = n();
        custom.apple = Some(AppleExtras {
            foreground: Some(ForegroundOptions {
                banner: false,
                list: true,
                sound: false,
                badge: true,
            }),
            ..AppleExtras::default()
        });
        assert_eq!(presentation(&custom, false), present::LIST | present::BADGE);
    }

    #[test]
    fn presentation_before_big_sur() {
        let full = presentation(&n(), false);
        assert_eq!(presentation_options(full, true), full);
        assert_eq!(
            presentation_options(full, false),
            present::ALERT | present::SOUND
        );
        // A silent update has no list-only option on 10.15.
        assert_eq!(presentation_options(presentation(&n(), true), false), 0);
        assert_eq!(
            presentation_options(present::LIST | present::BADGE, false),
            present::BADGE
        );
        assert_eq!(
            presentation_options(DEFAULT_PRESENTATION, false),
            present::ALERT | present::SOUND
        );
    }

    #[test]
    fn levels_and_sounds() {
        let none = Entitlements::default();
        let both = Entitlements {
            time_sensitive: true,
            critical: true,
        };
        let time_sensitive = Entitlements {
            time_sensitive: true,
            critical: false,
        };
        assert_eq!(interruption_level(None, false, both), 1);
        assert_eq!(
            interruption_level(Some(Interruption::Urgent), false, both),
            2
        );
        assert_eq!(
            interruption_level(Some(Interruption::Urgent), false, none),
            1
        );
        assert_eq!(
            interruption_level(Some(Interruption::Critical), false, both),
            3
        );
        // Without the entitlement each level falls back one step.
        assert_eq!(
            interruption_level(Some(Interruption::Critical), false, time_sensitive),
            2
        );
        assert_eq!(
            interruption_level(Some(Interruption::Critical), false, none),
            1
        );
        assert_eq!(
            interruption_level(Some(Interruption::Critical), true, both),
            0
        );
        let mut critical = n();
        critical.interruption = Some(Interruption::Critical);
        assert_eq!(sound(&critical, false, both), SoundChoice::Critical);
        assert_eq!(sound(&critical, false, none), SoundChoice::Default);
        assert_eq!(sound(&critical, true, both), SoundChoice::None);
        let mut named = n();
        named.sound = Some(Sound::Named("ding".to_owned()));
        assert_eq!(
            sound(&named, false, none),
            SoundChoice::Named("ding".to_owned())
        );
        named.sound = Some(Sound::Silent);
        assert_eq!(sound(&named, false, none), SoundChoice::None);
    }

    #[test]
    fn eviction_spares_categories_in_use() {
        let ours = ["a", "b", "c", "d"];
        let none = HashSet::new();
        assert_eq!(evict(&ours, 4, "d", &none), Vec::<&str>::new());
        assert_eq!(evict(&ours, 2, "d", &none), ["a", "b"]);
        let in_use: HashSet<String> = ["a".to_owned()].into();
        assert_eq!(evict(&ours, 2, "d", &in_use), ["b", "c"]);
        // Everything else in use: nothing goes, the cap gives.
        let all: HashSet<String> = ["a", "b", "c"].map(str::to_owned).into();
        assert_eq!(evict(&ours, 2, "d", &all), Vec::<&str>::new());
        // The one being registered is never dropped.
        assert_eq!(evict(&["x"], 0, "x", &none), Vec::<&str>::new());
    }

    #[test]
    fn image_types_from_content() {
        assert_eq!(
            image_extension(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"),
            Some("png")
        );
        assert_eq!(image_extension(&[0xff, 0xd8, 0xff, 0xe0]), Some("jpg"));
        assert_eq!(image_extension(b"GIF89a.."), Some("gif"));
        assert_eq!(image_extension(b"<svg"), None);
        assert_eq!(image_extension(b""), None);
    }

    #[test]
    fn launch_marks_are_bounded() {
        assert!(launched_app(1_000, 1_000));
        assert!(launched_app(1_000, 1_000 + LAUNCH_WINDOW_MS));
        assert!(!launched_app(1_000, 1_001 + LAUNCH_WINDOW_MS));
        assert!(!launched_app(0, 5));
        // A clock that went back does not claim the launch.
        assert!(!launched_app(1_000, 999));
        assert!(denied(1));
        assert!(!denied(0) && !denied(2));
    }

    #[test]
    fn categories_hash_content() {
        let limits = info("15.0", false).limits;
        let a = n().with_action(Action::button("a", "A").unwrap());
        let b = n().with_action(Action::button("b", "B").unwrap());
        let ra = render(&a, &limits);
        let rb = render(&b, &limits);
        assert_eq!(
            category_id(&a, &ra, None, false),
            category_id(&a, &ra, None, false)
        );
        assert_ne!(
            category_id(&a, &ra, None, false),
            category_id(&b, &rb, None, false)
        );
        assert_ne!(
            category_id(&a, &ra, None, false),
            category_id(&a, &ra, Some(LockScreen::HideContent), false)
        );
        assert!(category_id(&a, &ra, None, false).starts_with("istmo.c."));
        // Reply texts are part of the category: two placeholders, two ids.
        let reply = |placeholder: &str| {
            let mut action = Action::reply("r", "Reply").unwrap();
            if let crate::ActionKind::Reply(options) = &mut action.kind {
                options.placeholder = Some(placeholder.to_owned());
            }
            n().with_action(action)
        };
        let (x, y) = (reply("Say hi"), reply("Say bye"));
        let (rx, ry) = (render(&x, &limits), render(&y, &limits));
        assert_ne!(
            category_id(&x, &rx, None, false),
            category_id(&y, &ry, None, false)
        );
        assert_eq!(category_options(None), (1, None));
        assert_eq!(category_options(Some(LockScreen::Hidden)), (1, Some("")));
    }

    #[test]
    fn action_option_bits() {
        let limits = info("15.0", false).limits;
        let n = n().with_action(
            Action::button("del", "Delete")
                .unwrap()
                .destructive()
                .background(),
        );
        let rendered = render(&n, &limits);
        assert_eq!(action_options(&rendered.actions[0]), 0b10);
    }

    #[test]
    fn responses() {
        assert!(launched_by_notification(true, None));
        assert!(launched_by_notification(false, Some(false)));
        assert!(!launched_by_notification(false, Some(true)));
        assert!(!launched_by_notification(false, None));
        assert_eq!(response_kind(DISMISS_ACTION), ResponseKind::Dismissed);
        assert_eq!(response_kind(DEFAULT_ACTION), ResponseKind::Opened);
        assert_eq!(
            response_kind("reply"),
            ResponseKind::Button("reply".to_owned())
        );
    }

    #[test]
    fn permissions() {
        let status = permission(RawSettings {
            authorization: 2,
            alert: 2,
            sound: 1,
            badge: 2,
            time_sensitive: Some(2),
            direct_messages: None,
        });
        assert_eq!(status.state, PermissionState::Granted);
        assert!(status.alerts && !status.sound && status.badge);
        assert_eq!(status.time_sensitive, Some(true));
        assert_eq!(status.communication, None);
        assert_eq!(
            permission(RawSettings::default()).state,
            PermissionState::NotDetermined
        );
        let all = PermissionRequest {
            alerts: true,
            sound: true,
            badge: true,
            provisional: true,
            critical: true,
            ..PermissionRequest::default()
        };
        assert_eq!(authorization_options(all), 0b101_0111);
    }

    #[test]
    fn report_and_limits() {
        let plain = info("15.1", false);
        assert_eq!(plain.capabilities.backend, "macos:15.1");
        assert_eq!(
            plain.capabilities.support(Feature::Messaging),
            Support::Simulated
        );
        assert!(plain.limits.inline_reply && plain.limits.subtitle);
        assert!(!plain.limits.messaging && !plain.limits.choices);
        assert_eq!(plain.limits.max_actions, MAX_ACTIONS);
        let rich = info("15.1", true);
        assert_eq!(rich.capabilities.support(Feature::Avatar), Support::Native);
        assert!(rich.limits.messaging);
    }

    #[test]
    fn threads_and_senders() {
        let ana = Participant::new("ana", "Ana");
        let mut conversation = Conversation::new(Participant::new("me", "Me"));
        conversation.messages.push(ChatMessage::received(ana, "hi"));
        let chat = n().with_style(Style::Messaging(conversation.clone()));
        assert_eq!(thread_id(&chat), Some("n"));
        assert_eq!(
            communication_sender(&chat).map(|p| p.key.as_str()),
            Some("ana")
        );
        conversation.messages.push(ChatMessage::sent("yo"));
        let mine = n().with_style(Style::Messaging(conversation));
        assert!(communication_sender(&mine).is_none());
        assert_eq!(thread_id(&n()), None);
        assert_eq!(
            settings_url("a.b"),
            "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=a.b"
        );
    }
}
