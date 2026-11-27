//! Wire types of the notifications contract.
//!
//! Every type here crosses to Kotlin and Swift through the generated
//! codecs, so the shapes follow the extractor's rules: named-field
//! structs, enum variants with one unnamed payload, no variant named
//! after its payload type, `#[non_exhaustive]` only on enums the plugin
//! produces.

use istmo_macros::message;

use crate::ids::{ActionId, ChannelId, GroupId, NotificationId};

/// One notification: what [`crate::Notifications::show`] shows and
/// [`crate::Notifications::schedule`] stores.
///
/// Fields a platform cannot show are ignored there (see
/// [`Capabilities`]); only a method that exists for nothing else fails
/// with [`NotificationError::Unsupported`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    /// Identifies the notification; showing the same id again replaces it.
    pub id: NotificationId,
    /// Channel it belongs to; `None` = the `default` channel.
    pub channel: Option<ChannelId>,
    /// First line. Never empty.
    pub title: String,
    /// Second line (Apple subtitle, Windows second text line, appended
    /// to the body elsewhere).
    pub subtitle: Option<String>,
    /// Main text.
    pub body: Option<String>,
    /// Layout: plain, big text, inbox lines, picture, conversation, call.
    pub style: Style,
    /// Large icon, avatar or Windows `appLogoOverride`.
    pub icon: Option<NotificationImage>,
    /// Buttons, inline replies, choices and snoozes, in order.
    pub actions: Vec<Action>,
    /// Opened (as a deep link) when the user taps the notification.
    pub url: Option<String>,
    /// App data handed back in every response (at most 3 KiB).
    pub user_data: Option<String>,
    /// Group (Android group, Apple thread, Windows header).
    pub group: Option<Group>,
    /// What it is about; platforms rank and filter by it.
    pub category: Option<Category>,
    /// How much it interrupts; `None` = the channel's.
    pub interruption: Option<Interruption>,
    /// Sound; `None` = the channel's.
    pub sound: Option<Sound>,
    /// Progress bar (simulated as a text line where there is none).
    pub progress: Option<Progress>,
    /// The user cannot swipe it away (Android, Linux `resident`, web
    /// `requireInteraction`).
    pub ongoing: bool,
    /// Ask Android 16 QPR1+ to promote it as a Live Update.
    pub live: bool,
    /// Short status: the Android status bar chip, the Windows progress
    /// status.
    pub status_text: Option<String>,
    /// Removed after this many milliseconds.
    pub timeout_ms: Option<u64>,
    /// When the event happened, Unix-epoch milliseconds.
    pub timestamp_ms: Option<i64>,
    /// A running clock (Android chronometer).
    pub chronometer: Option<Chronometer>,
    /// Number shown on the notification (Android `setNumber`); not the
    /// app badge.
    pub count: Option<u32>,
    /// What the lock screen shows.
    pub lock_screen: Option<LockScreen>,
    /// Do not show while the app is in the foreground (Apple shows by
    /// default).
    pub hide_in_foreground: bool,
    /// What to do when a scheduled delivery was missed. Ignored by
    /// [`crate::Notifications::show`].
    pub missed: MissedPolicy,
    /// Android-only extras.
    pub android: Option<AndroidExtras>,
    /// Apple-only extras.
    pub apple: Option<AppleExtras>,
    /// Raw freedesktop hints, sent as given (escape hatch).
    pub linux_hints: Vec<LinuxHint>,
}

/// Android-only fields of a [`Notification`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags of a wire type"
)]
pub struct AndroidExtras {
    /// Drawable name of the small icon; default from
    /// `[app.notifications] small_icon`.
    pub small_icon: Option<String>,
    /// Accent colour, ARGB.
    pub color: Option<u32>,
    /// Fill the background with the accent colour.
    pub colorized: bool,
    /// Full-screen intent (needs the grant on Android 14+).
    pub full_screen: bool,
    /// Keep it after a tap (`!autoCancel`).
    pub keep_on_tap: bool,
    /// Order inside a group.
    pub sort_key: Option<String>,
    /// Do not bridge to wearables.
    pub local_only: bool,
}

/// Apple-only fields of a [`Notification`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppleExtras {
    /// `relevanceScore`, `0.0..=1.0` (iOS 15+).
    pub relevance_score: Option<f32>,
    /// `targetContentIdentifier`.
    pub target_content_id: Option<String>,
    /// How it shows while the app is in the foreground; `None` = banner,
    /// list and sound.
    pub foreground: Option<ForegroundOptions>,
    /// `filterCriteria` (Focus filters).
    pub filter_criteria: Option<String>,
}

/// How an Apple notification shows while the app is in the foreground.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags of a wire type"
)]
pub struct ForegroundOptions {
    /// Show the banner.
    pub banner: bool,
    /// Add it to the notification list.
    pub list: bool,
    /// Play the sound.
    pub sound: bool,
    /// Apply the badge.
    pub badge: bool,
}

impl Default for ForegroundOptions {
    fn default() -> Self {
        Self {
            banner: true,
            list: true,
            sound: true,
            badge: false,
        }
    }
}

/// One raw freedesktop notification hint.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LinuxHint {
    /// Hint name (`x-kde-urls`, `transient`, ...).
    pub key: String,
    /// Hint value.
    pub value: HintValue,
}

/// Value of a [`LinuxHint`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HintValue {
    /// D-Bus `b`.
    Bool(bool),
    /// D-Bus `i`.
    Number(i32),
    /// D-Bus `s`.
    Text(String),
    /// D-Bus `ay`.
    Bytes(Vec<u8>),
}

/// Layout of a [`Notification`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum Style {
    /// Title and body.
    #[default]
    Plain,
    /// Long text shown expanded.
    BigText(String),
    /// Up to a few short lines.
    Inbox(Vec<String>),
    /// A large picture.
    Picture(NotificationImage),
    /// A conversation (Android `MessagingStyle`, Apple communication
    /// notifications).
    Messaging(Conversation),
    /// An incoming or ongoing call.
    Call(CallInfo),
}

/// An image. Bytes are at most 2 MiB and are never decoded in Rust;
/// backends that need a file get one in the plugin's cache directory.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NotificationImage {
    /// PNG bytes.
    Png(Vec<u8>),
    /// JPEG bytes.
    Jpeg(Vec<u8>),
    /// Absolute path of an image file.
    File(String),
    /// Platform name: Android drawable, Apple bundle image, freedesktop
    /// icon name.
    Named(String),
}

/// Someone in a conversation or call.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Participant {
    /// Stable key (Android `Person.key`, Apple `INPersonHandle`).
    pub key: String,
    /// Display name.
    pub name: String,
    /// Avatar.
    pub avatar: Option<NotificationImage>,
    /// An automated sender.
    pub is_bot: bool,
    /// Android `Person.isImportant`.
    pub important: bool,
}

/// One message of a [`Conversation`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChatMessage {
    /// Text.
    pub text: String,
    /// When it was sent, Unix-epoch milliseconds.
    pub timestamp_ms: i64,
    /// Who sent it; `None` = the user.
    pub sender: Option<Participant>,
    /// Attached picture.
    pub image: Option<NotificationImage>,
}

/// A conversation shown with [`Style::Messaging`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Conversation {
    /// The user (the device owner).
    pub user: Participant,
    /// Conversation title (group chats).
    pub title: Option<String>,
    /// A group chat.
    pub is_group: bool,
    /// Messages, oldest first. The plugin keeps the last 25.
    pub messages: Vec<ChatMessage>,
}

/// A call shown with [`Style::Call`]. Its actions are the plugin's own
/// (`istmo.call.answer`, `istmo.call.decline`, `istmo.call.hang_up`).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallInfo {
    /// Who is calling.
    pub caller: Participant,
    /// Incoming, ongoing or screening.
    pub kind: CallKind,
    /// A video call.
    pub video: bool,
}

/// Phase of a [`CallInfo`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallKind {
    /// Ringing: answer and decline.
    Incoming,
    /// In progress: hang up.
    Ongoing,
    /// Being screened: answer and decline.
    Screening,
}

/// A button (or reply field, choice list, snooze) on a notification.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Action {
    /// Comes back in [`ResponseAction::Button`].
    pub id: ActionId,
    /// Label.
    pub title: String,
    /// Button, reply, choices or snooze.
    pub kind: ActionKind,
    /// Bring the app to the foreground; `false` = handled in the
    /// background (the response carries an [`AckToken`]).
    pub opens_app: bool,
    /// Shown as destructive.
    pub destructive: bool,
    /// Needs the device unlocked.
    pub requires_unlock: bool,
    /// Deep link published with the response; overrides the
    /// notification's.
    pub url: Option<String>,
    /// Icon of the button, where the platform shows one.
    pub icon: Option<NotificationImage>,
}

/// What an [`Action`] does.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ActionKind {
    /// A plain button.
    Button,
    /// An inline reply field.
    Reply(ReplyOptions),
    /// A fixed set of choices; the chosen one comes back as the reply.
    Select(Vec<String>),
    /// Hide and show again after this many milliseconds, without the
    /// app's code.
    Snooze(u64),
}

/// Options of an inline reply.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ReplyOptions {
    /// Placeholder of the text field.
    pub placeholder: Option<String>,
    /// Label of the send button.
    pub submit_title: Option<String>,
    /// Smart replies offered next to the field (Android `setChoices`).
    pub suggestions: Vec<String>,
}

/// Group of notifications.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Group {
    /// Identifies the group.
    pub id: GroupId,
    /// Group title (Windows header, Android summary title).
    pub title: Option<String>,
    /// Summary text (Android summary, Apple `summaryArgument`).
    pub summary: Option<String>,
}

/// What a notification is about.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    /// A message.
    Message,
    /// A call.
    Call,
    /// An alarm.
    Alarm,
    /// A reminder.
    Reminder,
    /// A calendar event.
    Event,
    /// A download, upload or sync.
    Transfer,
    /// Social network activity.
    Social,
    /// An error.
    Error,
    /// Ongoing status.
    Status,
    /// System or device status.
    System,
    /// A promotion.
    Promo,
}

/// How much a notification interrupts.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Interruption {
    /// Into the list, no sound, no banner.
    Passive,
    /// Default.
    Normal,
    /// Heads-up / banner with sound.
    High,
    /// Time sensitive: breaks through Focus where allowed.
    Urgent,
    /// Critical alert: needs the Apple entitlement, else [`Self::Urgent`].
    Critical,
}

/// Sound of a notification.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Sound {
    /// The platform default.
    Default,
    /// No sound.
    Silent,
    /// A sound bundled through `[app.notifications] sounds`, or a system
    /// sound name.
    Named(String),
}

/// What the lock screen shows.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LockScreen {
    /// Everything.
    Public,
    /// That there is a notification, not its content.
    HideContent,
    /// Nothing.
    Hidden,
}

/// A progress bar.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// `0.0..=1.0`; `None` = indeterminate.
    pub value: Option<f32>,
    /// Text next to the bar (Windows value string, the simulated line).
    pub label: Option<String>,
    /// Coloured segments (Android 16 `ProgressStyle`).
    pub segments: Vec<ProgressSegment>,
    /// Marked points (Android 16 `ProgressStyle`).
    pub points: Vec<ProgressPoint>,
}

/// A segment of an Android 16 progress bar.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgressSegment {
    /// Relative length.
    pub length: u32,
    /// ARGB colour.
    pub color: Option<u32>,
}

/// A point on an Android 16 progress bar.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgressPoint {
    /// Position, in the units of the segments' lengths.
    pub position: u32,
    /// ARGB colour.
    pub color: Option<u32>,
}

/// A running clock on the notification.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chronometer {
    /// Unix-epoch milliseconds the clock counts from (or to).
    pub base_ms: i64,
    /// Count down to `base_ms` instead of up from it.
    pub count_down: bool,
}

/// What to do with a scheduled notification whose time passed while it
/// could not be delivered (device off, app closed where scheduling runs
/// in-process).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MissedPolicy {
    /// Skip it (a repeating one waits for its next occurrence).
    Drop,
    /// Deliver it late when at most this many milliseconds late, once.
    DeliverLate(u64),
}

impl MissedPolicy {
    /// One hour: the default lateness.
    pub const DEFAULT_LATENESS_MS: u64 = 3_600_000;
}

impl Default for MissedPolicy {
    fn default() -> Self {
        Self::DeliverLate(Self::DEFAULT_LATENESS_MS)
    }
}

/// When a scheduled notification fires. Times are milliseconds.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Trigger {
    /// Once, this long after the call.
    After(u64),
    /// Once, at this Unix-epoch time.
    At(i64),
    /// Repeatedly, this often (at least 60 000), first one period after
    /// the call.
    Every(u64),
    /// On a calendar rule.
    Calendar(CalendarRule),
}

/// A wall-clock rule: `hour:minute` on the days that match every set
/// field.
///
/// A time that does not exist on a day (DST gap) fires as late as the
/// gap is long (`02:30` becomes `03:30`); a time that exists twice (DST
/// overlap) fires once, at the earlier one. A day that does not match
/// (`day_of_month = 31` in a 30-day month) is skipped. See
/// [`CalendarRule::next_after`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CalendarRule {
    /// `0..=23`.
    pub hour: u8,
    /// `0..=59`.
    pub minute: u8,
    /// Days of the week; empty = every day.
    pub weekdays: Vec<Weekday>,
    /// `1..=31`.
    pub day_of_month: Option<u8>,
    /// `1..=12`.
    pub month: Option<u8>,
    /// IANA zone name; `None` = the device's local wall clock (follows
    /// zone changes and DST).
    pub time_zone: Option<String>,
    /// No occurrence after this Unix-epoch time.
    pub until_ms: Option<i64>,
}

/// A day of the week.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Weekday {
    /// Monday.
    Mon,
    /// Tuesday.
    Tue,
    /// Wednesday.
    Wed,
    /// Thursday.
    Thu,
    /// Friday.
    Fri,
    /// Saturday.
    Sat,
    /// Sunday.
    Sun,
}

/// A change to a shown notification. Every `Some` replaces the field;
/// `clear` removes optional fields.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NotificationPatch {
    /// New title.
    pub title: Option<String>,
    /// New subtitle.
    pub subtitle: Option<String>,
    /// New body.
    pub body: Option<String>,
    /// New style.
    pub style: Option<Style>,
    /// New icon.
    pub icon: Option<NotificationImage>,
    /// New actions.
    pub actions: Option<Vec<Action>>,
    /// New progress.
    pub progress: Option<Progress>,
    /// New status text.
    pub status_text: Option<String>,
    /// New count.
    pub count: Option<u32>,
    /// New ongoing flag.
    pub ongoing: Option<bool>,
    /// New URL.
    pub url: Option<String>,
    /// New user data.
    pub user_data: Option<String>,
    /// Optional fields to remove.
    pub clear: Vec<PatchField>,
    /// Alert (sound, banner) again instead of updating silently.
    pub alert_again: bool,
}

/// An optional field [`NotificationPatch::clear`] removes.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PatchField {
    /// [`Notification::subtitle`].
    Subtitle,
    /// [`Notification::body`].
    Body,
    /// [`Notification::icon`].
    Icon,
    /// [`Notification::progress`].
    Progress,
    /// [`Notification::status_text`].
    StatusText,
    /// [`Notification::count`].
    Count,
    /// [`Notification::url`].
    Url,
    /// [`Notification::user_data`].
    UserData,
    /// [`Notification::actions`] (all of them).
    Actions,
}

/// A channel: importance, sound and lock-screen defaults for the
/// notifications in it. Android channels are immutable once created,
/// except name and description.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelSpec {
    /// Identifies the channel.
    pub id: ChannelId,
    /// Shown in the system settings.
    pub name: String,
    /// Shown in the system settings.
    pub description: Option<String>,
    /// Default interruption level.
    pub importance: Interruption,
    /// Default sound; `None` = the platform default.
    pub sound: Option<Sound>,
    /// Vibration pattern, milliseconds off / on.
    pub vibration: Option<Vec<u32>>,
    /// Notification light, ARGB.
    pub light_color: Option<u32>,
    /// Count towards the launcher badge.
    pub show_badge: bool,
    /// Default lock-screen visibility.
    pub lock_screen: Option<LockScreen>,
    /// Break through Do Not Disturb (needs the user's consent).
    pub bypass_dnd: bool,
    /// Channel group name.
    pub group: Option<String>,
}

/// A feature a platform may have, simulate or lack.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    /// Show now.
    Show,
    /// Update in place.
    Update,
    /// Update without alerting again.
    SilentUpdate,
    /// A real progress bar.
    ProgressBar,
    /// Android Live Update promotion.
    LivePromotion,
    /// Deliver at a time or after a delay.
    Schedule,
    /// Repeat (interval or calendar).
    Repeat,
    /// Deliver missed scheduled notifications.
    Missed,
    /// Action buttons.
    Actions,
    /// Inline reply.
    InlineReply,
    /// Choices.
    Choices,
    /// Snooze.
    Snooze,
    /// Avatar / large icon.
    Avatar,
    /// Big picture.
    Picture,
    /// Conversation layout.
    Messaging,
    /// Call layout.
    CallStyle,
    /// Groups / threads.
    Groups,
    /// Channels.
    Channels,
    /// Named sounds.
    CustomSound,
    /// Interruption levels.
    Interruption,
    /// Ongoing (not dismissable).
    Ongoing,
    /// Timeouts.
    Timeout,
    /// Lock-screen privacy.
    LockScreen,
    /// Timestamp / chronometer.
    Chronometer,
    /// List delivered notifications.
    ListDelivered,
    /// List scheduled notifications.
    ListPending,
    /// Hear about dismissals.
    Dismissal,
    /// Taps reach the app while it is closed.
    TapWhenClosed,
    /// Background actions and replies.
    BackgroundAction,
    /// Ask the user for permission.
    PermissionRequest,
    /// Exact alarms.
    ExactAlarms,
    /// Read the Do Not Disturb state.
    DoNotDisturb,
    /// Open the system settings.
    OpenSettings,
}

/// How a [`Feature`] is supported.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum Support {
    /// The platform does it.
    Native,
    /// The plugin simulates it.
    Simulated,
    /// Simulated, only while the app process runs.
    WhileRunning,
    /// Not available; the field is ignored.
    #[default]
    Unsupported,
}

/// One [`Feature`] and how it is supported.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FeatureSupport {
    /// The feature.
    pub feature: Feature,
    /// How it is supported.
    pub support: Support,
}

/// What the running backend does.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Capabilities {
    /// Backend and version: `android`, `fdo:mako 1.9`, `portal:2`,
    /// `windows:22631`, ...
    pub backend: String,
    /// Every feature the backend reports; a missing one is
    /// [`Support::Unsupported`].
    pub features: Vec<FeatureSupport>,
    /// Most actions shown on one notification (more become a "More"
    /// action).
    pub max_actions: u32,
    /// Most scheduled notifications kept, when limited.
    pub max_pending: Option<u32>,
    /// Most notifications shown at once, when limited.
    pub max_active: Option<u32>,
}

/// What [`crate::Notifications::request_permission`] asks for.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent flags of a wire type"
)]
pub struct PermissionRequest {
    /// Banners and the list.
    pub alerts: bool,
    /// Sounds.
    pub sound: bool,
    /// Badges.
    pub badge: bool,
    /// Apple provisional authorisation (quiet, no prompt).
    pub provisional: bool,
    /// Apple critical alerts (entitlement).
    pub critical: bool,
    /// Android exact alarms (opens the settings page on 12+).
    pub exact_alarms: bool,
    /// Android full-screen intents (opens the settings page on 14+).
    pub full_screen: bool,
    /// Android promoted notifications (opens the settings page).
    pub promoted: bool,
}

impl Default for PermissionRequest {
    /// Alerts, sound and badge.
    fn default() -> Self {
        Self {
            alerts: true,
            sound: true,
            badge: true,
            provisional: false,
            critical: false,
            exact_alarms: false,
            full_screen: false,
            promoted: false,
        }
    }
}

/// Whether the app may notify.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PermissionState {
    /// Allowed.
    Granted,
    /// Refused.
    Denied,
    /// Never asked.
    NotDetermined,
    /// Apple provisional (quiet) authorisation.
    Provisional,
    /// Apple App Clip ephemeral authorisation.
    Ephemeral,
    /// Nothing to ask for (Linux, Android below 13).
    NotRequired,
    /// Notifications do not work here.
    Unavailable,
}

/// Permission state with the per-feature details a platform reports.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PermissionStatus {
    /// Overall state.
    pub state: PermissionState,
    /// Banners and the list are allowed.
    pub alerts: bool,
    /// Sounds are allowed.
    pub sound: bool,
    /// Badges are allowed.
    pub badge: bool,
    /// Android exact alarms.
    pub exact_alarms: Option<bool>,
    /// Android full-screen intents.
    pub full_screen: Option<bool>,
    /// Android promoted notifications.
    pub promoted: Option<bool>,
    /// Apple time-sensitive notifications.
    pub time_sensitive: Option<bool>,
    /// Apple communication notifications.
    pub communication: Option<bool>,
    /// Do Not Disturb is on.
    pub do_not_disturb: Option<bool>,
}

impl PermissionStatus {
    /// A status in `state` with no per-feature details; alerts, sound and
    /// badge follow `state` (allowed for granted, provisional, ephemeral
    /// and not required).
    #[must_use]
    pub const fn of(state: PermissionState) -> Self {
        let allowed = matches!(
            state,
            PermissionState::Granted
                | PermissionState::Provisional
                | PermissionState::Ephemeral
                | PermissionState::NotRequired
        );
        Self {
            state,
            alerts: allowed,
            sound: allowed,
            badge: allowed,
            exact_alarms: None,
            full_screen: None,
            promoted: None,
            time_sensitive: None,
            communication: None,
            do_not_disturb: None,
        }
    }

    /// Whether notifications can be shown.
    #[must_use]
    pub const fn allows_alerts(&self) -> bool {
        self.alerts
    }
}

/// Result of [`crate::Notifications::schedule`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScheduledInfo {
    /// Fires at the exact time (Android without the exact-alarm grant
    /// fires within a window).
    pub exact: bool,
    /// Next delivery, Unix-epoch milliseconds; `None` when it will never
    /// fire (a rule past its `until_ms`).
    pub next_fire_ms: Option<i64>,
}

/// A scheduled notification, as [`crate::Notifications::pending`] lists it.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
pub struct PendingNotification {
    /// The notification.
    pub notification: Notification,
    /// Its trigger.
    pub trigger: Trigger,
    /// Next delivery, Unix-epoch milliseconds.
    pub next_fire_ms: Option<i64>,
}

/// A shown notification, as [`crate::Notifications::delivered`] lists it.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeliveredNotification {
    /// Its id.
    pub id: NotificationId,
    /// Title, when the platform reports it.
    pub title: Option<String>,
    /// Body, when the platform reports it.
    pub body: Option<String>,
    /// Group, when the platform reports it.
    pub group: Option<GroupId>,
    /// When it was shown, Unix-epoch milliseconds.
    pub delivered_ms: Option<i64>,
}

/// Hands a background response back: call
/// [`crate::Notifications::complete`] with it once done, so the OS may
/// suspend the app again.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AckToken {
    /// Opaque.
    pub value: u64,
}

/// What [`crate::Notifications::events`] reports.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NotificationEvent {
    /// Shown (now or on its schedule).
    Delivered(NotificationId),
    /// A missed scheduled notification was shown late.
    DeliveredLate(NotificationId),
    /// Gone from the screen without a tap.
    Dismissed(Dismissal),
    /// Snoozed by the user; it comes back on its own.
    Snoozed(NotificationId),
    /// Could not be shown.
    Failed(Failure),
    /// Asked for Live Update promotion, not promoted.
    NotPromoted(NotificationId),
}

/// A dismissal.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Dismissal {
    /// Which notification.
    pub id: NotificationId,
    /// Why.
    pub reason: DismissReason,
}

/// Why a notification went away.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DismissReason {
    /// The user dismissed it.
    User,
    /// It timed out.
    Timeout,
    /// The app removed it.
    App,
    /// The platform did not say.
    Other,
}

/// A notification that could not be shown.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Failure {
    /// Which notification, when known.
    pub id: Option<NotificationId>,
    /// What the platform said.
    pub message: String,
}

/// Why a call failed.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NotificationError {
    /// The platform (or backend) cannot do this.
    Unsupported(Feature),
    /// The user did not allow notifications.
    PermissionDenied,
    /// macOS: the app is not a bundled, signed `.app`.
    NotBundled,
    /// Windows: elevated processes cannot show toasts.
    Elevated,
    /// A field is malformed.
    Invalid(InvalidField),
    /// Over a platform limit; carries the limit.
    TooMany(u32),
    /// No channel with this id.
    UnknownChannel(String),
    /// No notification (or ack token) with this id.
    NotFound(String),
    /// Any other platform failure.
    Platform(PlatformError),
}

/// A malformed field.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InvalidField {
    /// Path of the field: `title`, `actions[2].id`, `trigger.hour`.
    pub field: String,
    /// What is wrong with it.
    pub problem: Problem,
}

/// What is wrong with an [`InvalidField`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Problem {
    /// Empty where a value is required.
    Empty,
    /// Longer than its limit.
    TooLong,
    /// Characters outside the allowed set.
    BadCharset,
    /// Repeats another value that must be unique.
    Duplicate,
    /// Not an acceptable URL.
    BadUrl,
    /// Bytes over their limit.
    TooLarge,
    /// A number outside its range.
    OutOfRange,
    /// Uses a name reserved by the plugin.
    Reserved,
    /// Not allowed in this place or combination.
    NotAllowedHere,
}

/// A platform failure.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlatformError {
    /// Platform code (`HRESULT`, `NSError.code`, ...); `0` when none.
    pub code: i64,
    /// Platform message.
    pub message: String,
}

/// Which part of a notification a response came from.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ResponseAction {
    /// The notification itself was tapped.
    Open,
    /// An action.
    Button(ActionId),
    /// Dismissed (where the platform reports it as a response).
    Dismiss,
    /// The simulated "More" action: the app shows
    /// [`NotificationResponse::overflow_actions`].
    More,
}

/// What the user did with a notification. Published on the
/// `istmo.notifications.responses` queue; read it with
/// [`crate::NotificationResponses`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NotificationResponse {
    /// Which notification.
    pub id: NotificationId,
    /// Which part of it.
    pub action: ResponseAction,
    /// Inline reply text or the chosen choice.
    pub reply: Option<String>,
    /// The reply (or choices) was simulated with a button: the app shows
    /// its own field.
    pub input_requested: bool,
    /// The notification's user data.
    pub user_data: Option<String>,
    /// The action's URL, else the notification's.
    pub url: Option<String>,
    /// This response started the process.
    pub launched_app: bool,
    /// For [`ResponseAction::More`]: the actions the platform did not show.
    pub overflow_actions: Vec<Action>,
    /// Background response: call [`crate::Notifications::complete`] with
    /// it when done. `None` for foreground responses.
    pub ack: Option<AckToken>,
}

impl std::fmt::Display for Feature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "empty",
            Self::TooLong => "too long",
            Self::BadCharset => "characters outside the allowed set",
            Self::Duplicate => "duplicate",
            Self::BadUrl => "not an acceptable URL",
            Self::TooLarge => "too large",
            Self::OutOfRange => "out of range",
            Self::Reserved => "reserved by the plugin",
            Self::NotAllowedHere => "not allowed here",
        })
    }
}

impl InvalidField {
    /// `field` has `problem`.
    #[must_use]
    pub fn new(field: impl Into<String>, problem: Problem) -> Self {
        Self {
            field: field.into(),
            problem,
        }
    }
}

impl std::fmt::Display for InvalidField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid notification field `{}`: {}",
            self.field, self.problem
        )
    }
}

impl std::error::Error for InvalidField {}

impl PlatformError {
    /// A failure with no platform code.
    #[must_use]
    pub fn message(message: impl Into<String>) -> Self {
        Self {
            code: 0,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.code == 0 {
            f.write_str(&self.message)
        } else {
            write!(f, "{} (code {})", self.message, self.code)
        }
    }
}

impl std::error::Error for PlatformError {}

impl std::fmt::Display for NotificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported(feature) => write!(f, "not supported here: {feature}"),
            Self::PermissionDenied => f.write_str("notification permission denied"),
            Self::NotBundled => {
                f.write_str("notifications need a bundled, signed .app with a bundle id")
            }
            Self::Elevated => f.write_str("elevated processes cannot show notifications"),
            Self::Invalid(field) => field.fmt(f),
            Self::TooMany(limit) => write!(f, "over the platform limit of {limit}"),
            Self::UnknownChannel(id) => write!(f, "unknown notification channel `{id}`"),
            Self::NotFound(id) => write!(f, "no notification `{id}`"),
            Self::Platform(err) => write!(f, "notification backend error: {err}"),
        }
    }
}

impl std::error::Error for NotificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(field) => Some(field),
            Self::Platform(err) => Some(err),
            _ => None,
        }
    }
}

impl From<InvalidField> for NotificationError {
    fn from(field: InvalidField) -> Self {
        Self::Invalid(field)
    }
}

/// Recover the typed error from a failed client call.
impl From<istmo_core::IstmoError> for NotificationError {
    fn from(err: istmo_core::IstmoError) -> Self {
        match err {
            istmo_core::IstmoError::PluginError { bytes } => {
                istmo_core::codec::decode::<Self>(&bytes).map_or_else(
                    |e| {
                        Self::Platform(PlatformError::message(format!(
                            "decode plugin error payload: {e}"
                        )))
                    },
                    |(err, _)| err,
                )
            }
            other => Self::Platform(PlatformError::message(other.to_string())),
        }
    }
}
