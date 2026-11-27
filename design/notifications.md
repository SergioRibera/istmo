# `istmo-notifications`

Local notifications on every istmo target, with live updates. Replaces
the built-in `istmo.notifications` contract in `istmo-plugins` (four
methods, no shipped backend, natives only inside `rust-mobile-demo`).
Same plugin id, no compatibility shim (nothing has shipped).

Goal: one model that works everywhere. When a platform has a feature, use
it. When it does not, the plugin simulates it (and says so in
`capabilities`). Only refuse when there is no honest way to do it.

Push (APNs / FCM / Web Push) is a separate plugin
(`istmo-push`, later): it needs servers, entitlements and service
extensions. This plugin is what a push handler calls to show something.

## 1. What each platform offers

### Android (NotificationCompat, API 21+; minSdk of istmo)

- Channels (26+): importance, sound, vibration pattern, lights, badge,
  lock-screen visibility, bypass DND; immutable after creation except
  name / description. Channel groups. Per-channel blocked state.
- Styles: BigText, BigPicture, Inbox, Messaging (conversations,
  `Person`, shortcuts for bubbles), Call (31+), Media (out of scope),
  ProgressStyle (36: segments, points, tracker icon).
- Live Updates (Android 16 QPR1+): `setRequestPromotedOngoing`, ongoing,
  title, style in {BigText, Call, Progress, Metric}, manifest permission
  `POST_PROMOTED_NOTIFICATIONS` (normal permission); status bar chip text
  `setShortCriticalText`.
- Updates in place: same id + tag; `setOnlyAlertOnce` keeps them silent.
  The system drops updates above about 5 per second per app.
- Progress `setProgress(max, value, indeterminate)` on any style.
- Actions (max 3 shown), `RemoteInput` inline reply with `setChoices`
  (smart choices), semantic action, `setAuthenticationRequired` (31+).
- Groups + summary (the app must post the summary; the system
  auto-groups at 4+).
- `setTimeoutAfter`, `setOngoing`, `setWhen` / `showWhen`,
  `setUsesChronometer` + `setChronometerCountDown` (24+), `setNumber`
  (launcher badge count), `setColor`, `setColorized`, category,
  `setVisibility` + `setPublicVersion`, full-screen intent (14+ needs
  `USE_FULL_SCREEN_INTENT` grant), delete intent (dismissal).
- Listing: `getActiveNotifications` (23+). No listing of scheduled ones
  (there is no OS scheduler for notifications).
- Scheduling: none native. `AlarmManager` (`setExactAndAllowWhileIdle`
  needs `SCHEDULE_EXACT_ALARM` / `USE_EXACT_ALARM`; inexact otherwise),
  alarms lost on reboot and app update (`BOOT_COMPLETED`,
  `TIME_SET`, `TIMEZONE_CHANGED`), OEM caps on alarm count (Samsung 500); 14+: `SCHEDULE_EXACT_ALARM` denied by default, `USE_EXACT_ALARM` Play-policy gated.
- Permission: `POST_NOTIFICATIONS` runtime (33+);
  `areNotificationsEnabled`; `currentInterruptionFilter` (DND state).
- Settings: `ACTION_APP_NOTIFICATION_SETTINGS`,
  `ACTION_CHANNEL_NOTIFICATION_SETTINGS`.

### iOS / iPadOS (UserNotifications)

- Content: title, subtitle, body, badge, sound (default, named bundle
  sound, critical with entitlement), attachments (image / audio / video
  file URLs, copied by the system), `threadIdentifier` (grouping),
  `summaryArgument`, `relevanceScore` (15+), `interruptionLevel` (15+:
  passive, active, timeSensitive (entitlement), critical (entitlement)),
  `targetContentIdentifier`, `userInfo`.
- Categories with actions (registered up front with
  `setNotificationCategories`): button / `UNTextInputNotificationAction`,
  options foreground / destructive / authenticationRequired, action
  icons (15+), hidden-preview placeholder, custom dismiss action
  (needed to hear about dismissals).
- Triggers: time interval (repeat >= 60 s), calendar (`DateComponents`,
  repeats), location (needs location permission; out of v1).
- 64 pending requests per app, the rest silently dropped.
- Update: adding a request with an existing identifier replaces it,
  delivered or pending. It alerts again (no "only alert once"); with no
  sound and `.passive` it updates the list quietly (to verify on device).
- Listing: pending and delivered requests, remove by identifier.
- Foreground: nothing shows unless the delegate's `willPresent` returns
  options. Responses (tap, action, reply) only through the delegate,
  which must be set before launch finishes (cold start).
- Communication notifications (`INSendMessageIntent`, entitlement),
  Live Activities (the `istmo-live-activity` plugin), notification
  service / content extensions (push only).
- Badge: `setBadgeCount` (16+), else `applicationIconBadgeNumber`.
- Settings: `openNotificationSettingsURLString` (16+), else app settings.

### macOS (UserNotifications, 10.14+)

Same API as iOS (no Live Activities, no critical without entitlement,
time-sensitive needs entitlement). Only works from a bundled, signed
`.app` with a bundle id: an unbundled binary throws or crashes inside
`UNUserNotificationCenter.current()`. Responses relaunch the app
(Apple Event + delegate). Deprecated `NSUserNotification` is not a way
out (same bundle requirement, removed APIs).

### Windows 10 1809+ / 11 (WinRT `Windows.UI.Notifications`)

- Toast XML: text lines, app logo override (circle crop), hero image,
  inline images, attribution text, custom timestamp, header (groups
  toasts in Action Center), up to 5 buttons, text input boxes, selection
  boxes (choices), context menu items, system snooze / dismiss actions,
  audio (`ms-winsoundevent:` built-ins, loop; `ms-appx` custom audio only for
  packaged apps), scenarios (`reminder`, `alarm`, `incomingCall`,
  `urgent` (11)), duration long/short.
- Progress bar with data binding (title, value, value string override,
  status); updated in place with `ToastNotifier.Update(NotificationData,
  tag, group)` and a sequence number: no re-popup, cheapest live update
  of any platform.
- Tag + group identify a toast; `SuppressPopup` posts straight to Action
  Center; `ExpirationTime`; `ExpiresOnReboot`; priority High.
- `ScheduledToastNotification` (fires while the app is closed; no
  repeats); `GetScheduledToastNotifications` / remove.
- History: `ToastNotificationHistory` get / remove (tag, group) / clear.
- Events while running: Activated, Dismissed (UserCanceled,
  ApplicationHidden, TimedOut), Failed.
- Activation when closed: COM activator implementing
  `INotificationActivationCallback`, registered under HKCU
  `Software\Classes\AppUserModelId\<AUMID>` (`DisplayName`, `IconUri`,
  `CustomActivator`) plus `Software\Classes\CLSID\{clsid}\LocalServer32`.
  Older guidance also asks for a Start-menu shortcut carrying the AUMID
  and the activator CLSID. Packaged (MSIX): manifest
  `com:Extension` / `desktop:ToastNotificationActivation`.
- Permission: `ToastNotifier.Setting` (Enabled, DisabledForApplication,
  DisabledForUser, DisabledByGroupPolicy, DisabledByManifest). No prompt.
- Badge: packaged only (`BadgeUpdateManager`); not this plugin.
- Settings: `ms-settings:notifications`.

### Linux: `org.freedesktop.Notifications` (spec 1.2)

- `Notify(app_name, replaces_id, app_icon, summary, body, actions,
  hints, expire_timeout)`: `replaces_id` = update in place.
- Hints: `urgency` (low/normal/critical), `category`, `image-data` /
  `image-path`, `sound-file` / `sound-name` / `suppress-sound`,
  `resident`, `transient`, `action-icons`, `desktop-entry`,
  `x-kde-*`, `value` (0-100 progress, de-facto: dunst, mako, swaync, xfce4-notifyd; not Plasma, not GNOME).
- `GetCapabilities`: `actions`, `body`, `body-markup`, `body-images`,
  `icon-static`, `persistence`, `sound`, `inline-reply` (KDE),
  `x-kde-urls`, ... Feature support depends on the running server.
- Signals: `ActionInvoked`, `NotificationClosed(reason)`,
  `ActivationToken` (focus stealing), `NotificationReplied` (KDE inline
  reply). Only while the app is running: the server never starts it.
- No scheduling, no listing, no permission.
- KDE progress jobs: bus name `org.kde.kuiserver`, interface `org.kde.JobViewServerV2` (real progress UI with
  speed, cancel / suspend); better than the `value` hint on Plasma.

### Linux: portal `org.freedesktop.portal.Notification` (Flatpak, Snap, host apps that register)

- v1: title, body, icon, priority (low/normal/high/urgent),
  default-action + target, buttons (label, action, target).
  `AddNotification(id, dict)` with the same id replaces.
  `ActionInvoked(id, action, parameter)`; the portal can activate the
  app through D-Bus activation when it is not running (needs
  `DBusActivatable=true` and the `org.freedesktop.Application`
  interface).
- v2: markup-body, sound (default / silent / file / bytes), icon as
  themed name or file descriptor, buttons with `purpose` (e.g. call
  accept/decline; exact purpose names to verify), `display-hint`
  (transient, tray, persistent, hide-on-lock-screen,
  hide-content-on-lock-screen, show-as-new), `category`
  (`im.received`, `call.incoming`, `call.ongoing`, `call.unanswered`, `alarm.ringing`, `os.battery.low`, ...). Supported
  options read from `SupportedOptions`.
- No closed signal, no listing, no scheduling. Host (non-sandboxed) apps
  must register an app id (`org.freedesktop.host.portal.Registry`).

### Web (Notification API + Service Worker)

- Page `new Notification(title, opts)`: body, icon, tag (replace),
  `renotify`, `silent`, `requireInteraction`, `data`, `lang`, `dir`,
  `timestamp`, vibrate (mobile). Click / close / error / show events.
- Service worker `registration.showNotification`: also `actions`
  (`Notification.maxActions`, 2 on Chrome), `image`, `badge`;
  `getNotifications({ tag })` lists shown ones; `notificationclick`
  works with the page closed.
- `Notification.requestPermission()` needs a user gesture.
- No scheduling (Notification Triggers was abandoned), no progress, no
  input (except Chrome's `type: "text"` action, non-standard).


### Corrections from review (supersede anything above)

- Portal v2 has native inline reply: button `purpose = "im.reply-with-text"`
  (category `im.received`), text arrives in the `parameter` of the
  activation. Call purposes: `call.accept`, `call.decline`,
  `call.hang-up`, `call.enable-speakerphone`, `call.disable-speakerphone`.
  Buttons with a purpose omit the label; supported purposes, categories
  and options come from `SupportedOptions`.
- Portal actions: only actions named `app.<name>` are "exported"; the
  portal calls `org.freedesktop.Application.ActivateAction("<name>",
  [target], platform_data)` on the app's bus name (D-Bus activating it if
  needed: `.service` file + `DBusActivatable=true`) and does **not** emit
  `ActionInvoked` for them. Non-exported actions only emit
  `ActionInvoked` (app running).
- KDE inline reply (fdo): capability `inline-reply`, an action whose key
  is `inline-reply`, hints `x-kde-reply-placeholder-text` /
  `x-kde-reply-submit-button-text`, signal `NotificationReplied(u, s)`.
  swaync implements it too.
- Android Live Updates (16 QPR1+): styles Standard / BigText / Call /
  Progress / Metric; must be ongoing, have a title, no `RemoteViews`, not a
  group summary, not colorized, channel not `IMPORTANCE_MIN`.
  `NotificationManager.canPostPromotedNotifications()`,
  `Settings.ACTION_MANAGE_APP_PROMOTED_NOTIFICATIONS`,
  `Notification.FLAG_PROMOTED_ONGOING` tell whether it was promoted. OEMs
  may add criteria.
- Android rate limit is per package (about 5 enqueues per second), not
  per notification; about 50 active notifications per app.
- Android 12+: every `PendingIntent` `FLAG_IMMUTABLE` except the reply
  action (`FLAG_MUTABLE` + explicit intent, 14 refuses implicit mutable
  ones); foreground actions use `getActivity` directly (notification
  trampoline restriction).
- Apple communication notifications work for local notifications without
  a service extension (iOS 15 / macOS 12): capability
  `com.apple.developer.usernotifications.communication`,
  `INSendMessageIntent` in the app's `NSUserActivityTypes`, `INPerson`
  with `INPersonHandle` + `INImage`, stable `conversationIdentifier`,
  `INInteraction(direction: .incoming)` donated (wait for success), then
  `content.updating(from: intent)`. Calls: `INStartCallIntent`.
  Device check: a forum thread reports missing avatars on local ones.
- Apple `setNotificationCategories` replaces the whole set.
  `willPresent` never runs in the background.
- Web: on Chrome Android `new Notification()` throws: mobile web needs
  the service worker even to show. Firefox / Safari: no `actions`,
  `image`, `badge`. iOS Safari only from home-screen web apps (16.4+).
  Gesture requirement for permission: Firefox and Safari.
- Windows (research 2026-11-27, see section 6): registry-only
  registration (no Start-menu shortcut) is what Windows App SDK,
  Community Toolkit 7 and Firefox ship; minimum supported build 17763
  (1809). Unpackaged images: local absolute paths only. Custom audio:
  only `ms-appx` / `ms-resource` (packaged), so unpackaged apps get
  `ms-winsoundevent:` sounds or silence. Max 5 buttons (context menu
  items included), 5 inputs. Elevated processes are unsupported.

## 2. Feature matrix

`N` native, `S` simulated by the plugin, `R` only while the app process
runs (simulated), `-` unsupported (the capability says so; the field is
ignored, never an error unless the method exists only for it).

| Feature | Android | iOS | macOS | Windows | Linux fdo | Linux portal | Web |
|---|---|---|---|---|---|---|---|
| show now | N | N | N (bundle) | N | N | N | N (SW on mobile) |
| update in place | N | N (re-add) | N (re-add) | N | N (`replaces_id`) | N (same id) | N (tag) |
| silent update | N (alert once) | S (passive, no sound) | S | N (binding / SuppressPopup) | N | N | N (`renotify=false`) |
| progress bar | N (ProgressStyle 36+, else `setProgress`) | S (text) | S (text) | N (bar) | N (`value` on dunst/mako/swaync/xfce) / N (KDE job) / S (text) | S (text) | S (text) |
| live update promotion | N (16 QPR1+, eligibility) | - (see live-activity) | - | - | - | - | - |
| schedule at / after | N (AlarmManager, one alarm armed) | N | N | N (ScheduledToast) | R | R | R |
| repeat daily / weekly / interval | N (Kotlin rule eval) | N (64 cap, top-up) | N | S (window re-armed) | R | R | R |
| missed while off | S (boot / time change re-arm) | N | N | N | S (catch-up policy at start) | S | S |
| actions | N (3) | N (4) | N (10) | N (5) | N if `actions` cap | N | N (SW, maxActions; not FF/Safari) |
| more actions than max | S (`More`) | S | S | S | S | S | S |
| inline reply | N | N | N | N | N (`inline-reply` cap) else S | N (v2 purpose) else S | S |
| choices | N (`setChoices`) | S (buttons) | S | N (selection) | S | S | S |
| snooze | S | S | S | N (system) | S | S | S |
| avatar / large icon | N (`Person.setIcon`, large icon) | N (communication capability) else S (attachment) | same as iOS | N (`appLogoOverride` circle) | N (`image-path`) | N (icon fd / bytes) | N (`icon`) |
| picture | N (BigPicture) | N (attachment) | N | N (hero) | N (`image-path`) | - | N (`image`, Chromium) |
| big text / inbox | N | S (body) | S | S (lines) | S (body) | S | S |
| messaging style | N | N (capability) else S (body) | same | S (lines) | S (body) | S (`im.received`) | S |
| call style | N (CallStyle) | N (capability, `INStartCallIntent`) else S | same | N (`incomingCall`) | S (critical + buttons) | N (`call.incoming` + purposes) | S |
| groups / threads | N (+ S summary) | N (thread) | N | N (group + header) | - | - | - |
| channels | N | S (defaults) | S | S | S | S | S |
| sound default / silent / named | N (channel) | N | N | N system names only | N (`sound-name`) | N (v2) | silent only |
| interruption level | N (importance) | N (15+) | N | S (scenario) | N (urgency) | N (priority) | S (`requireInteraction`) |
| ongoing / persistent | N | - | - | S (`reminder` scenario) | N (`resident`) | N (`persistent`) | N (`requireInteraction`) |
| timeout | N | R | R | N (expiration) | N | R | R |
| lock-screen privacy | N | S (category placeholder) | S | - | - | N (v2 hints) | - |
| timestamp / chronometer | N | - | - | N (timestamp) | - | - | N (timestamp) |
| list delivered | N | N | N | N (history) | R (own table) | R | N (SW) |
| list scheduled | N (own store) | N | N | N | R | R | R |
| dismissal event | N (delete intent) | N (custom dismiss) | N | R | R | - | N |
| tap when app closed | N | N | N | N (COM activator) | - | N (exported actions) | N (SW) |
| background reply / action | N (receiver + `goAsync`) | N (completion handler) | N | N (COM, process started) | R | N (D-Bus activation) | N (SW) |
| permission request | N (33+) | N | N | - (read only) | - | - | N (gesture on FF/Safari) |
| exact alarms permission | N (12+) | - | - | - | - | - | - |
| DND state | N | - | - | - | - | - | - |
| open settings | N (app / channel / exact alarm / promoted) | N | N | N | - | - | - |

### Simulations, precisely

- **Progress as text**: `<label> 42%` (or `<label>` when indeterminate)
  appended as the last body line; updates take the silent path.
- **Live update coalescing** (one `Coalescer`, Rust on desktop/web, a
  Kotlin and a Swift port on mobile): per id the latest pending update
  wins; a per-app token bucket limits posts (Android 4/s with burst 4,
  Apple 2/s, Windows binding 20/s, Linux 10/s, web 4/s). Alerting updates
  (`alert_again`) and the final `finish` jump the queue. `cancel` drops
  queued updates for that id. An update for a notification the user
  dismissed is dropped (dismissals tracked from delete intents / custom
  dismiss / `NotificationClosed` / Windows `Dismissed`) unless
  `alert_again`.
- **Scheduling engine**:
  - Rule math lives in `CalendarRule::next_after(instant, tz)` (Rust,
    `jiff`) and its Kotlin port (`java.util.Calendar` + `TimeZone`, no
    desugaring, minSdk 21). Both pinned by
    `tests/schedule_vectors.tsv` (DST gaps and overlaps, leap days,
    month-end, weekday sets, `until`), like the webview rule vectors.
    `time_zone: None` = local wall clock, follows DST.
  - Android: Kotlin `ScheduleStore` (file in `noBackupFilesDir`, format
    byte, entries are generated-codec bytes of `(Notification,
    Trigger)`), exactly one `AlarmManager` alarm armed for the soonest
    entry (`setExactAndAllowWhileIdle` with the grant, else
    `setAndAllowWhileIdle`: `ScheduledInfo.exact` says which), re-armed on
    fire, `BOOT_COMPLETED`, `TIME_SET`, `TIMEZONE_CHANGED`,
    `MY_PACKAGE_REPLACED`, `SCHEDULE_EXACT_ALARM_PERMISSION_STATE_CHANGED`.
    No Rust needed at fire time.
  - Apple: native triggers (`UNCalendarNotificationTrigger` repeats,
    interval >= 60 s). All requests also kept in the plugin store; the
    soonest 60 are armed; top-up on launch, on foreground and in the
    delegate callbacks.
  - Windows: `ScheduledToastNotification` for one-shots; repeats armed
    as the next occurrences in a 14-day window (cap 50), re-armed on every
    start.
  - Linux / web: in-process engine, one timer for the soonest entry (one
    shared timer thread on desktop, `setTimeout` on web), store in the
    plugin data dir (desktop: format byte + wincode, atomic
    `fsx`-style write, only the `LocalEndpoint` owner writes; web:
    IndexedDB). `MissedPolicy` applied at start. `Support::WhileRunning`.
- **More actions than the platform shows**: the first `max - 1` stay,
  the rest become a `More` action that opens the app with
  `ResponseAction::More` and `overflow_actions`.
- **Reply / choices where missing**: a button that opens the app with
  `input_requested = true`.
- **Snooze**: `ActionKind::Snooze(ms)`, handled without the app's code
  (Android receiver, Apple background action, Windows system snooze,
  desktop in-process), then a `Snoozed` event.
- **Android group summary**: posted by the plugin while a group has 2+
  members; removed with the last one.
- **Channels off Android**: defaults merged into each notification.
- **Delivered list where the OS has none**: the plugin's own table.

Rejected (do not re-propose): drawing progress into images (plugin never
draws); per-notification sound on Android through derived channels;
decoding / scaling images in Rust (no `image` crate: bytes pass through,
size-capped, natives scale); `osascript` for unbundled macOS (user
decision: bundle required, README says so); a Start-menu shortcut on
Windows (registry-only route, 1809+); Windows builds before 17763.

## 3. Responses

```rust
#[non_exhaustive]
pub enum ResponseAction { Open, Button(ActionId), Dismiss, More }

pub struct NotificationResponse {
    pub id: NotificationId,
    pub action: ResponseAction,
    pub reply: Option<String>,          // inline reply text or chosen choice
    pub input_requested: bool,          // reply/choices simulated: app shows its field
    pub user_data: Option<String>,
    pub url: Option<String>,            // action url, else notification url
    pub launched_app: bool,             // this response started the process
    pub overflow_actions: Vec<Action>,
    pub ack: Option<AckToken>,          // background action: call `complete(ack)` when done
}
```

- Published on the early queue `istmo.notifications.responses`
  (`PreMainQueue`, capacity 64) and, when `url` is set, also on
  `istmo.deeplinks` with `source = notification:<id>[:<action>]`.
- Reader `NotificationResponses` (hand-written like `DeepLinks`):
  `acquire()` / `from_runtime`, `stream()` with `recv` / `recv_async` /
  `try_recv`; `NotificationResponse::kind()` gives a flat
  `Response::{Open, Action(&str), Reply(&str), Dismissed, More}` view.
- Background actions (`opens_app = false`): the OS keeps the process
  alive only briefly. Android: `IstmoNotificationReceiver` uses
  `goAsync()`, starts the runtime if needed (`IstmoRuntime.ensureStarted`
  from the receiver context), publishes, and finishes when the app calls
  `complete(ack)` or after 9 s. Apple: the delegate holds the completion
  handler until `complete(ack)` or 25 s. Windows: COM `Activate` returns
  at once; the process stays until the app is done. Linux portal: D-Bus
  activation starts the app. `ack` is `None` for foreground responses.
- Cold start per platform:

| Platform | Closed | Running |
|---|---|---|
| Android | `IstmoNotificationActivity` (not exported, `getActivity`, foreground) / `IstmoNotificationReceiver` (background, snooze, dismiss): queued in a native file, drained into the lane when the runtime starts | same, published directly |
| iOS | delegate installed by the generated registry (`IstmoNotifications.install()`); apps with their own delegate forward `IstmoNotifications.didReceive(_:completion:)` and `willPresent(_:completion:)` | delegate |
| macOS | delegate installed at `applicationWillFinishLaunching` (observer, like autostart) | delegate |
| Windows | COM activator (`-istmo-notification -Embedding`); if another instance owns the `LocalEndpoint`, forward and exit | COM `Activate` in the primary |
| Linux portal | exported actions + D-Bus activation when the plugin can own `org.freedesktop.Application` for the app id; else non-exported actions (running only) | `ActivateAction` / `ActionInvoked` |
| Linux fdo | lost (`Support::Unsupported` for tap-when-closed) | `ActionInvoked` / `NotificationReplied` |
| Web | service worker focuses / opens the page and posts the response | `click` / SW message |

`user_data` <= 3 KiB (Windows `arguments`, Android extras, Apple
`userInfo` all carry it).

## 4. Wire types

Rules: named-field structs; enum variants carry one unnamed payload; no
variant named after its payload type; `#[non_exhaustive]` only on enums
that are outputs (an `Unknown` mid-struct would break every later
field); Kotlin top-level names avoid `Person`, `Message`, `Channel`,
`Image` (androidx / coroutines clashes). Swift lives in the
`Notifications` namespace (no reserved-name clash).

```rust
pub struct NotificationId { pub value: String }   // 1..=64 [A-Za-z0-9._:-]; TryFrom<&str>
pub struct ChannelId { pub value: String }        // same rule
pub struct ActionId { pub value: String }          // same rule; `istmo.` prefix reserved
pub struct GroupId { pub value: String }           // same rule

pub struct Notification {
    pub id: NotificationId,
    pub channel: Option<ChannelId>,                // None = "default" channel
    pub title: String,
    pub subtitle: Option<String>,
    pub body: Option<String>,
    pub style: Style,
    pub icon: Option<NotificationImage>,           // large icon / avatar / appLogoOverride
    pub actions: Vec<Action>,
    pub url: Option<String>,
    pub user_data: Option<String>,
    pub group: Option<Group>,
    pub category: Option<Category>,
    pub interruption: Option<Interruption>,        // None = channel's
    pub sound: Option<Sound>,                      // None = channel's
    pub progress: Option<Progress>,
    pub ongoing: bool,
    pub live: bool,                                // ask for promotion (Android 16 QPR1+)
    pub status_text: Option<String>,               // Android chip, Windows progress status
    pub timeout_ms: Option<u64>,
    pub timestamp_ms: Option<i64>,
    pub chronometer: Option<Chronometer>,
    pub count: Option<u32>,                        // Android setNumber; NOT the app badge
    pub lock_screen: Option<LockScreen>,
    pub hide_in_foreground: bool,                  // default false: Apple shows while open
    pub missed: MissedPolicy,                      // scheduled only
    pub android: Option<AndroidExtras>,
    pub apple: Option<AppleExtras>,
    pub linux_hints: Vec<LinuxHint>,               // raw fdo hints escape hatch
}

pub struct AndroidExtras {
    pub small_icon: Option<String>,   // drawable name; default from [app.notifications] small_icon
    pub color: Option<u32>,           // ARGB accent
    pub colorized: bool,
    pub full_screen: bool,            // full-screen intent (needs grant on 14+)
    pub keep_on_tap: bool,            // !autoCancel
    pub sort_key: Option<String>,
    pub local_only: bool,
}
pub struct AppleExtras {
    pub relevance_score: Option<f32>,
    pub target_content_id: Option<String>,
    pub foreground: Option<ForegroundOptions>,     // None = banner + list + sound
    pub filter_criteria: Option<String>,
}
pub struct ForegroundOptions { pub banner: bool, pub list: bool, pub sound: bool, pub badge: bool }
pub struct LinuxHint { pub key: String, pub value: HintValue }
pub enum HintValue { Bool(bool), Int(i32), Text(String), Bytes(Vec<u8>) }

pub enum Style {
    Plain,
    BigText(String),
    Inbox(Vec<String>),
    Picture(NotificationImage),
    Messaging(Conversation),
    Call(CallInfo),
}
pub enum NotificationImage { Png(Vec<u8>), Jpeg(Vec<u8>), File(String), Named(String) }
// bytes <= 2 MiB, refused above (InvalidField TooLarge); never decoded in Rust.
// Paths the OS needs are written to the plugin cache dir, named by content hash,
// removed when no live notification uses them.

pub struct Participant { pub key: String, pub name: String, pub avatar: Option<NotificationImage>,
                         pub is_bot: bool, pub important: bool }
pub struct ChatMessage { pub text: String, pub timestamp_ms: i64,
                         pub sender: Option<Participant>,   // None = the user
                         pub image: Option<NotificationImage> }
pub struct Conversation { pub user: Participant, pub title: Option<String>,
                          pub is_group: bool, pub messages: Vec<ChatMessage> }   // plugin keeps <= 25
pub struct CallInfo { pub caller: Participant, pub kind: CallKind, pub video: bool }
pub enum CallKind { Incoming, Ongoing, Screening }
// Call actions have reserved ids istmo.call.answer / istmo.call.decline / istmo.call.hang_up.

pub struct Action {
    pub id: ActionId,
    pub title: String,
    pub kind: ActionKind,
    pub opens_app: bool,           // false = background (ack token)
    pub destructive: bool,
    pub requires_unlock: bool,
    pub url: Option<String>,
    pub icon: Option<NotificationImage>,
}
pub enum ActionKind { Button, Reply(ReplyOptions), Select(Vec<String>), Snooze(u64) }
pub struct ReplyOptions { pub placeholder: Option<String>, pub submit_title: Option<String>,
                          pub suggestions: Vec<String> }

pub struct Group { pub id: GroupId, pub title: Option<String>, pub summary: Option<String> }
pub enum Category { Message, Call, Alarm, Reminder, Event, Transfer, Social, Error, Status, System, Promo }
pub enum Interruption { Passive, Normal, High, Urgent, Critical }   // Critical needs the Apple entitlement, else Urgent
pub enum Sound { Default, Silent, Named(String) }  // bundled ([app.notifications] sounds) or system names
pub enum LockScreen { Public, HideContent, Hidden }
pub struct Progress { pub value: Option<f32>,      // None = indeterminate, else 0.0..=1.0
                      pub label: Option<String>,
                      pub segments: Vec<ProgressSegment>, pub points: Vec<ProgressPoint> }  // Android 16
pub struct ProgressSegment { pub length: u32, pub color: Option<u32> }
pub struct ProgressPoint { pub position: u32, pub color: Option<u32> }
pub struct Chronometer { pub base_ms: i64, pub count_down: bool }
pub enum MissedPolicy { Drop, DeliverLate(u64) }   // max lateness ms; default DeliverLate(1 h)

pub enum Trigger { After(u64), At(i64), Every(u64), Calendar(CalendarRule) }   // ms; Every >= 60 000
pub struct CalendarRule { pub hour: u8, pub minute: u8, pub weekdays: Vec<Weekday>,   // empty = every day
                          pub day_of_month: Option<u8>, pub month: Option<u8>,
                          pub time_zone: Option<String>,   // IANA; None = local wall clock
                          pub until_ms: Option<i64> }
pub enum Weekday { Mon, Tue, Wed, Thu, Fri, Sat, Sun }

pub struct NotificationPatch {          // every Some replaces; `clear` removes optional fields
    pub title: Option<String>, pub subtitle: Option<String>, pub body: Option<String>,
    pub style: Option<Style>, pub icon: Option<NotificationImage>, pub actions: Option<Vec<Action>>,
    pub progress: Option<Progress>, pub status_text: Option<String>, pub count: Option<u32>,
    pub ongoing: Option<bool>, pub url: Option<String>, pub user_data: Option<String>,
    pub clear: Vec<PatchField>, pub alert_again: bool,
}
pub enum PatchField { Subtitle, Body, Icon, Progress, StatusText, Count, Url, UserData, Actions }

pub struct ChannelSpec { pub id: ChannelId, pub name: String, pub description: Option<String>,
                         pub importance: Interruption, pub sound: Option<Sound>,
                         pub vibration: Option<Vec<u32>>, pub light_color: Option<u32>,
                         pub show_badge: bool, pub lock_screen: Option<LockScreen>,
                         pub bypass_dnd: bool, pub group: Option<String> }

#[non_exhaustive] pub enum Feature { Show, Update, SilentUpdate, ProgressBar, LivePromotion, Schedule,
    Repeat, Missed, Actions, InlineReply, Choices, Snooze, Avatar, Picture, Messaging, CallStyle,
    Groups, Channels, CustomSound, Interruption, Ongoing, Timeout, LockScreen, Chronometer,
    ListDelivered, ListPending, Dismissal, TapWhenClosed, BackgroundAction, PermissionRequest,
    ExactAlarms, DoNotDisturb, OpenSettings }
#[non_exhaustive] pub enum Support { Native, Simulated, WhileRunning, Unsupported }
pub struct FeatureSupport { pub feature: Feature, pub support: Support }
pub struct Capabilities { pub backend: String,        // "android", "fdo:mako 1.9", "portal:2", "windows:22631", ...
                          pub features: Vec<FeatureSupport>,
                          pub max_actions: u32, pub max_pending: Option<u32>, pub max_active: Option<u32> }
// Rust sugar: Capabilities::support(Feature) -> Support.

pub struct PermissionRequest { pub alerts: bool, pub sound: bool, pub badge: bool, pub provisional: bool,
                               pub critical: bool, pub exact_alarms: bool, pub full_screen: bool,
                               pub promoted: bool }
#[non_exhaustive] pub enum PermissionState { Granted, Denied, NotDetermined, Provisional, Ephemeral,
                                             NotRequired, Unavailable }
pub struct PermissionStatus { pub state: PermissionState, pub alerts: bool, pub sound: bool, pub badge: bool,
                              pub exact_alarms: Option<bool>, pub full_screen: Option<bool>,
                              pub promoted: Option<bool>, pub time_sensitive: Option<bool>,
                              pub communication: Option<bool>, pub do_not_disturb: Option<bool> }

pub struct ScheduledInfo { pub exact: bool, pub next_fire_ms: Option<i64> }
pub struct PendingNotification { pub notification: Notification, pub trigger: Trigger, pub next_fire_ms: Option<i64> }
pub struct DeliveredNotification { pub id: NotificationId, pub title: Option<String>, pub body: Option<String>,
                                   pub group: Option<GroupId>, pub delivered_ms: Option<i64> }
pub struct AckToken { pub value: u64 }

#[non_exhaustive] pub enum NotificationEvent { Delivered(NotificationId), DeliveredLate(NotificationId),
    Dismissed(Dismissal), Snoozed(NotificationId), Failed(Failure), NotPromoted(NotificationId) }
pub struct Dismissal { pub id: NotificationId, pub reason: DismissReason }
#[non_exhaustive] pub enum DismissReason { User, Timeout, App, Unknown }
pub struct Failure { pub id: Option<NotificationId>, pub message: String }

#[non_exhaustive] pub enum NotificationError { Unsupported(Feature), PermissionDenied, NotBundled, Elevated,
    InvalidField(InvalidField), TooMany(u32), UnknownChannel(String), NotFound(String), Platform(PlatformError) }
pub struct InvalidField { pub field: String, pub problem: Problem }
#[non_exhaustive] pub enum Problem { Empty, TooLong, BadCharset, Duplicate, BadUrl, TooLarge, OutOfRange,
                                     Reserved, NotAllowedHere }
pub struct PlatformError { pub code: i64, pub message: String }
```

## 5. Trait

```rust
#[plugin(name = "istmo.notifications", remote = false)]
pub trait Notifications {
    async fn capabilities(&self) -> Capabilities;
    async fn permission(&self) -> PermissionStatus;
    async fn request_permission(&self, request: PermissionRequest) -> Result<PermissionStatus, NotificationError>;
    async fn open_settings(&self, channel: Option<ChannelId>) -> Result<(), NotificationError>;
    async fn set_channels(&self, channels: Vec<ChannelSpec>) -> Result<(), NotificationError>;
    async fn delete_channel(&self, id: ChannelId) -> Result<(), NotificationError>;
    async fn show(&self, notification: Notification) -> Result<(), NotificationError>;
    async fn schedule(&self, notification: Notification, trigger: Trigger) -> Result<ScheduledInfo, NotificationError>;
    async fn update(&self, id: NotificationId, patch: NotificationPatch) -> Result<(), NotificationError>;
    async fn set_progress(&self, id: NotificationId, progress: Option<Progress>) -> Result<(), NotificationError>;
    async fn append_message(&self, id: NotificationId, message: ChatMessage) -> Result<(), NotificationError>;
    async fn cancel(&self, id: NotificationId) -> Result<(), NotificationError>;
    async fn cancel_group(&self, group: GroupId) -> Result<(), NotificationError>;
    async fn cancel_all(&self) -> Result<(), NotificationError>;
    async fn pending(&self) -> Result<Vec<PendingNotification>, NotificationError>;
    async fn delivered(&self) -> Result<Vec<DeliveredNotification>, NotificationError>;
    async fn complete(&self, ack: AckToken) -> Result<(), NotificationError>;
    #[stream]
    fn events(&self) -> NotificationEvent;
}
```

`remote = false`: responses are delivered in the main process (the
trampoline / delegate / activator run there). `Notification::validate`
and `NotificationPatch::validate` run on the Rust client before the call
and again in the Rust desktop host; natives re-check platform limits.

### Rust sugar (client side, no wire impact)

```rust
let n = Notification::new("dl:42", "Downloading report.pdf")?   // id validated here
    .with_body("3.1 of 8 MB")
    .with_channel("downloads")?
    .with_progress(Progress::fraction(0.38))
    .with_action(Action::button("pause", "Pause")?)
    .with_action(Action::button("cancel", "Cancel")?.destructive())
    .ongoing();

let live = client.show_live(n).await?;     // LiveNotification
live.progress(0.4);                         // sync, coalesced, never awaits
live.finish(Notification::new("dl:42", "Done")?.with_url("app://files/42")).await?;
// dropped without finish: cancel (opt-out: live.detach())

client.show_simple("hello", "Title", "Body").await?;
client.schedule(n, Trigger::daily(8, 0)).await?;          // Trigger::after(Duration), ::at(SystemTime), ::weekly(..)
client.ensure_permission(PermissionRequest::default()).await?;   // asks only when NotDetermined
client.append_message("chat:42", ChatMessage::from(ana.clone(), "On my way"))?;
Image: NotificationImage::png(bytes), ::jpeg(bytes), ::file(path), ::named("ic_chat");
Participant::new("ana", "Ana").with_avatar(NotificationImage::png(avatar));

let responses = NotificationResponses::acquire()?.stream();
while let Ok(r) = responses.recv_async().await {
    match r.kind() {
        Response::Reply(text) => chat.send(&r.id, text).await,
        Response::Action("mark_read") => chat.mark_read(&r.id).await,
        Response::Open => router.open(&r.id),
        _ => {}
    }
    if let Some(ack) = r.ack { client.complete(ack).await?; }
}
```

`LiveNotification` uses one shared sender per client (no thread per
notification): `progress` stores the latest value in a slot and wakes
the sender, which keeps one call in flight. `CalendarRule::next_occurrences(from, n)`
is public so apps can test DST themselves. `FakeNotifications` (feature
`testing`) implements the trait, records calls, takes
`with_capabilities` / `with_permission`, and `respond(id, Response)`
feeds the responses lane.

App badge: owned by `istmo-launcher-actions::set_badge`. `count` is only
the per-notification number; Apple `content.badge` is never set by this
plugin.

## 6. Configuration (`istmo.toml`) and platform setup

```toml
[app.notifications]
small_icon   = "assets/notif.xml"   # Android: .xml vector or alpha-only .png (build rejects opaque colour); default: [app.icon.android] monochrome
accent       = "#E0662B"
sounds       = ["assets/ding.wav"]  # Android res/raw, Apple bundle (caf/wav/aiff <= 30 s), Linux next to the exe data; Sound::Named("ding")
exact_alarms = "none"               # "schedule" -> SCHEDULE_EXACT_ALARM, "use" -> USE_EXACT_ALARM (alarm / calendar apps only)
full_screen  = false                # USE_FULL_SCREEN_INTENT
live_updates = false                # POST_PROMOTED_NOTIFICATIONS
apple = { time_sensitive = false, communication = false, critical = false }  # entitlements + NSUserActivityTypes
web_service_worker = true           # stage istmo-notifications-sw.js

[[app.notification_channel]]
id = "messages"
name = "Messages"
description = "New messages"
importance = "high"
sound = "ding"
```

- Static channels are created by the native backend at start (Android
  reads a generated `res/raw/istmo_notification_channels.bin`, wire bytes
  of `Vec<ChannelSpec>`); `set_channels` adds dynamic ones (one per chat).
- Android manifest (plugin fragment, merged by the Gradle plugin):
  `POST_NOTIFICATIONS`, `RECEIVE_BOOT_COMPLETED`, receivers and the
  trampoline activity; the opt-in permissions above come from the app
  table.
- Windows: AUMID = `[app] id` (<= 128 chars), display name `[app] name`,
  icon = the desktop icon from `[app.icon]` written next to the exe.
  `DesktopNotifications::install()` (called from the desktop runtime
  setup, like `DocumentsPublisher`) does, on every start:
  1. refuse elevated processes (`Elevated`), builds < 17763 (`Unsupported`);
  2. packaged (`GetCurrentApplicationUserModelId` ok) → no registry, the
     manifest's `com:ExeServer` + `desktop:ToastNotificationActivation`
     (written by packaging) carry activation;
  3. unpackaged → `SetCurrentProcessExplicitAppUserModelID(aumid)`, write
     `HKCU\Software\Classes\AppUserModelId\<aumid>` (`DisplayName`,
     `IconUri`, `IconBackgroundColor`, `CustomActivator = {clsid}`) and
     `HKCU\Software\Classes\CLSID\{clsid}\LocalServer32` = `"<exe>"
     -istmo-notification` (COM appends `-Embedding`); CLSID = UUID v5 of
     the AUMID;
  4. register the COM class (`CoRegisterClassObject`, `CLSCTX_LOCAL_SERVER`,
     `REGCLS_MULTIPLEUSE`) on a dedicated MTA thread, early (COM waits at
     most 60 s); strip `-istmo-notification` / `-Embedding` from argv; a
     COM-launched process that is not the `LocalEndpoint` owner forwards
     the activation and exits; console-subsystem exes call `FreeConsole`
     when COM-launched.
  `DesktopNotifications::unregister()` clears history, scheduled toasts
  and both keys (for uninstallers). History / remove / clear always pass
  the AUMID.
- Linux: app id = `FLATPAK_ID`, else `with_app_id`, else the exe stem
  (same rule as launcher-actions). Portal when sandboxed or when the
  host registry accepts the app id; fdo otherwise. For exported portal
  actions the plugin owns `<app id>` and serves
  `org.freedesktop.Application` on the derived path only when the name
  is free (a GApplication app owns it already: then non-exported actions);
  packaging writes the D-Bus `.service` file and `DBusActivatable=true`.
- macOS: bundle required; `NotBundled` everywhere otherwise, logged once
  at warn; README and `istmo doctor` say so.
- iOS: zero native code by default; the generated registry installs the
  delegate.
- Web: `importScripts("istmo-notifications-sw.js")` for apps with their
  own service worker; otherwise the plugin registers its own.

## 7. Crate layout

```
plugins/notifications/
  istmo.toml           id istmo.notifications, android_package dev.istmo.plugins.notifications,
                       gradle androidx.core, android permissions, manifest fragment
  src/lib.rs           wire types, trait, UnavailableNotifications
  src/ids.rs           id newtypes + validation
  src/builder.rs       Notification / Action / Participant / Trigger sugar
  src/validate.rs      Notification::validate, NotificationPatch::validate (limits table)
  src/render.rs        fallbacks: progress text, styles to body lines, overflow, choices
  src/coalesce.rs      per-id latest + per-app token bucket
  src/schedule/        CalendarRule math (jiff), store, timer engine, missed policy
  src/responses.rs     NotificationResponses lane reader, deep-link publishing
  src/live.rs          LiveNotification + shared sender
  src/fake.rs          FakeNotifications (feature testing)
  src/desktop/linux/   fdo.rs, portal.rs, kde_job.rs, app_activation.rs
  src/desktop/windows/ toast_xml.rs, notifier.rs, activator.rs, registration.rs   (allow unsafe_code)
  src/desktop/macos.rs UNUserNotificationCenter + Intents (allow unsafe_code)
  src/web.rs           Notification API / service worker bridge
  native/android/      NotificationsBackendImpl, IstmoNotificationActivity, IstmoNotificationReceiver,
                       AlarmReceiver, BootReceiver, ScheduleStore, CalendarRule.kt, Coalescer.kt, AndroidManifest.xml
  native/ios/          NotificationsBackendImpl, IstmoNotifications (delegate, install), Categories,
                       Communication (Intents), ScheduleStore, Coalescer
  assets/web/          istmo-notifications-sw.js
  tests/               validation, builder, render, coalescer, schedule vectors (Rust), fdo + portal +
                       kuiserver against a private dbus-daemon, toast XML goldens, codegen goldens,
                       native wire check
crates/build/src/notifications.rs   [app.notifications] + [[app.notification_channel]]
```

Lints: `unsafe_code = "deny"`, allowed only on `desktop::windows` and
`desktop::macos` (reason per module).

## 8. Implementation order

1. Core crate: wire types, ids, builders, validation, render, coalescer,
   schedule math + vectors, responses lane, live handle, fake, desktop host
   skeleton with the in-process scheduler, `UnavailableNotifications`,
   codegen goldens.
2. `istmo-build` config + Android resources / manifest / Apple
   entitlements.
3. In parallel: Linux; Android; Apple (iOS Swift + macOS Rust); Windows;
   Web.
4. Remove the old `istmo.notifications` contract (istmo-plugins,
   istmo-plugins-schema, tests, rust-mobile-demo backends) in the same
   change that ports the demo; docs en/es, README, CHANGELOG.
5. Review: one adversarial review per platform + one cross-platform
   behaviour review.

## Open

- Hardware: everything native (Android, iOS, macOS, Windows). Windows:
  registry-only on 1809 and current 11, Action Center persistence,
  COM cold start race with the single-instance claim, `ToastNotifier.Update`
  and history without the private FrameworkUdk step, same AUMID from two
  exe paths.
- Apple: silent replace behaviour, communication avatars on local
  notifications.
- Android OEM promotion criteria; OEM alarm behaviour.

## Implementation notes (core, as built)

Branch `notifications`, steps 1 and 4 of section 8. Crate
`plugins/notifications` (`istmo-notifications`); the old contract is gone
from `istmo-plugins` / `istmo-plugins-schema` and `rust-mobile-demo`
uses the new crate (its Kotlin / Swift backends were deleted; the demo
logs the failure and carries on until the natives land).

### Layout

- `src/types.rs` (wire types), `src/ids.rs` (id newtypes): the extractor
  follows `mod` files, so the wire types do not have to sit in `lib.rs`.
- `src/builder.rs` (constructors, `with_*`, `Notification::apply`),
  `src/client.rs` (`*_checked`, `show_simple`, `ensure_permission`,
  `show_live`), `src/validate.rs` (limits table in its module docs),
  `src/render.rs`, `src/coalesce.rs`, `src/schedule.rs` +
  `schedule/{rule,engine,store}.rs`, `src/responses.rs`, `src/live.rs`,
  `src/unavailable.rs`, `src/fake.rs` (feature `testing`),
  `src/desktop.rs` + `desktop/{backend,images,timer,tests}.rs`
  (`desktop.rs` + `desktop/`, the repo convention, not `desktop/mod.rs`),
  `src/web.rs` (module boundary only).
- `istmo.toml`: id, `client_type`, `android_package =
  "dev.istmo.plugins.notifications"`, `auto_register = false` (no
  `NotificationsBackendImpl` exists yet; the Android and Apple agents
  turn it back on with their `android_backend` / registry entries),
  `[min_versions]` android 21, ios 14.0.
- New dependency: `jiff = "1.0"` (default features: tzdb bundled where
  the OS has none; no serde), `features = ["js"]` on wasm32.
  `wasm-bindgen-futures` on wasm32 for the live sender.

### Deviations from sections 4 and 5

- `NotificationError::InvalidField(InvalidField)` is
  `NotificationError::Invalid(InvalidField)`: never name a variant after
  its payload type (Kotlin nested class shadows the type).
- `DismissReason::Unknown` is `DismissReason::Other`: a `non_exhaustive`
  enum may not have a variant named `Unknown` (`ContractError::ReservedUnknown`).
- `HintValue::Int(i32)` is `HintValue::Number(i32)`: a Kotlin nested
  class `Int` shadows `kotlin.Int` inside `HintValue` (kotlinc rejected
  the generated codec). Rule: no variant named like a Kotlin builtin
  (`Int`, `Long`, `Float`, `Double`, `Boolean`, `String`, `Unit`).
- Every unary trait method takes `ctx: HostCtx` (stripped from the wire,
  invisible to Kotlin / Swift): the desktop host binds the runtime of the
  first call, so a backend activation can publish on the responses lane.
  `DesktopNotifications::attach(&runtime)` binds it earlier (cold start).
- Every method that may touch the OS is `#[istmo::blocking]` (all but
  `complete` and `events`): D-Bus, WinRT and file writes.
- `ChatMessage::from(sender, text)` is `ChatMessage::received(sender,
  text)` / `ChatMessage::sent(text)` (an inherent two-argument `from`
  collides with `From::from`).
- The generated client owns the method names, so the validated sugar is
  `show_checked`, `schedule_checked`, `update_checked(&str, patch)`,
  `append_message_checked(&str, msg)` (async; section 5's sync
  `append_message("chat:42", ..)?` does not exist).
- `Trigger::daily` / `weekly` / `after` / `at` / `every` do not validate
  (they return `Trigger`); `schedule_checked` and the hosts do.
- `NotificationResponses::stream()` returns its own
  `NotificationResponseStream` (`recv` / `recv_async` / `try_recv`):
  `istmo_plugins::EarlyStream::new` is crate-private.
- `NotificationResponse`, `ResponseAction`, `AckToken` are
  `#[message]`s, so natives get generated codecs for the responses lane
  (`writeNotificationResponse`) instead of hand-encoding it.
- `Response` (the flat view) is `#[non_exhaustive]`.
- `FakeNotifications::respond(id, Response)` returns
  `Result<NotificationResponse, NotificationError>` and needs a runtime
  (`attach(&rt)`, or any call through a runtime binds it).
- `UnavailableNotifications::request_permission` answers
  `Ok(Unavailable)` (not an error), like `permission`.
- Extra helpers: `Capabilities::none` / `has`, `PermissionStatus::of`,
  `NotificationPatch::progress` / `alerting`, `Notification::normalize`
  (keeps the last 25 messages), `CalendarRule::{daily, on, on_day,
  in_month, in_zone, until}`, `Trigger::{first_fire, next_after, repeats}`
  plus `*_in(.., &jiff::tz::TimeZone)` variants that stand in for the
  device zone (tests); jiff is a public dependency of those functions.
- Unknown IANA zone: `InvalidField { field: "trigger.time_zone",
  problem: OutOfRange }`.

### Desktop `Backend` contract (`src/desktop/backend.rs`)

The Linux, Windows and macOS agents implement `Backend` and replace
`platform_backend` in `src/desktop.rs` with their `cfg` arms (it returns
`Err(Unsupported(Show))` today, so `DesktopNotifications::default()`
behaves like `UnavailableNotifications`).

- `info() -> BackendInfo { capabilities, limits: RenderLimits, rate:
  Rate, lists_delivered, timeouts }`. Read once after the start and again
  after `BackendSink::changed()` (see "Review fixes").
  Report only what the platform does; `desktop::overlay` adds the host's
  simulations (Schedule / Repeat / Snooze `WhileRunning`, Missed /
  ListPending / Channels / ProgressBar / Messaging `Simulated`,
  InlineReply / Choices / CallStyle `Simulated` when actions exist,
  ListDelivered / Timeout `WhileRunning` unless the flags say native).
- `show(&Shown { notification, rendered, images, mode })`: the
  notification is validated, normalised and has the channel defaults
  merged (interruption, sound, lock screen). `rendered.body` already
  carries every simulated line (subtitle when `limits.subtitle` is
  false, style lines, progress text when `limits.progress_bar` is
  false); `rendered.actions` are the buttons to show, each with a `key`
  the platform action must carry and hand back. `mode` is `New` or
  `Replace { alert }` (`alert = false` = silent update: fdo
  `replaces_id` + `suppress-sound`, Windows data binding / SuppressPopup,
  Apple passive re-add).
- `remove(id)`, `permission()`, `request_permission()`,
  `open_settings()`, `set_channels()`, `delivered()` (only when
  `lists_delivered`), `complete(ack)` (release whatever keeps the
  process alive for a background response) have defaults.
- Report back through the `BackendSink` handed to the start function:
  `activated(Activation { id, key, reply, launched_app, carried })`,
  `dismissed(id, reason)`, `failed(id, message)`, `changed()` (server
  restarted: `info()` is read again). `carried` holds user data / URL
  the platform activation itself carries (Windows toast arguments,
  portal targets) for notifications shown by an earlier process.
- Threads: backend methods are called from blocking workers and the
  host's timer thread, never with a host lock held; sink calls may come
  from any thread, also from inside a backend method.
- Keys: action id; `istmo.more`; `<action id>#<choice index>` for a
  choice simulated as a button. `Rendered::respond` resolves them
  (snooze keys resolve to `Resolved::Snooze`, handled by the host:
  removed, re-scheduled with `Trigger::After`, `Snoozed` event).
- Background actions (`opens_app = false`) get an `AckToken` from the
  host; `complete(ack)` removes it and calls `Backend::complete`.

### Host behaviour (shared by every desktop backend)

- One timer thread (`istmo-notifications-timer`) for scheduled entries,
  queued coalesced updates and host-run timeouts; it re-reads the wall
  clock at least every 30 s (suspend, clock changes). Hosts built with
  `manual_timer()` wait for `tick()` (tests).
- Delivered table: latest content per id (queued updates included);
  dismissed entries are kept (at most 256) so an alerting update can
  show them again (`ShowMode::New`).
- Unknown channels fail `UnknownChannel`; `with_channels` on the builder
  takes the static ones (`[[app.notification_channel]]`, wiring left to
  the istmo-build agent). `set_channels` keeps an existing channel's
  settings except name and description (the Android rule everywhere).
- Store: `<data dir>/istmo-notifications/schedule.bin`, one writer
  (`without_store()` in forwarding processes). Writes are atomic
  (sibling `.tmp`, `sync_all`, rename), serialised by a generation
  counter so a slow older write never lands after a newer one; an empty
  schedule removes the file.

### Image cache rules

- `<cache dir>/istmo-notifications/images/<fnv1a64 hex>-<len>.<png|jpg>`
  for `NotificationImage::Png` / `Jpeg`; `File` paths are passed as
  given; `Named` has no file. Slots: `Icon`, `Picture`, `Avatar(key)`
  (conversation user, senders, caller; once per key), `Message(index)`,
  `Action(id)`.
- Reference counted across notifications; the file goes with its last
  user (replaced with different images, cancelled, dismissed). Images
  are collected again only when an update touches icon, style or actions
  (progress updates reuse the files).
- At start, files older than 3 days that nothing uses are removed
  (younger ones may still back notifications an earlier process left on
  screen).

### Ports the native agents own

- **Coalescer** (Kotlin `Coalescer.kt`, Swift `Coalescer.swift`):
  `src/coalesce.rs` is the reference; its unit tests are the cases to
  port. Integer milli-token bucket (`Rate { per_second, burst }`, refill
  `elapsed_ms * per_second` milli-tokens, cap `burst * 1000`), latest per
  id, FIFO of ids, regular offers queue behind queued ids, `Alert` /
  `Finish` post at once and spend a token (debt allowed, floored at
  `-burst` tokens), `shown`
  (a full show) clears dismissal and the queue entry and spends a token,
  `dismissed` drops regular updates until an alert, `cancel` forgets the
  id, `next_due` rounds the wait up. Rates: `Rate::ANDROID` (4, 4),
  `APPLE` (2, 2), `WINDOWS` (20, 20), `LINUX` (10, 10), `WEB` (4, 4).
- **Calendar rule** (Kotlin `CalendarRule.kt`; Swift only if it ever
  evaluates rules itself instead of `UNCalendarNotificationTrigger`):
  `src/schedule/rule.rs`, pinned by `tests/schedule_vectors.tsv`.
  Semantics: AND of the set fields, gap = shift forward by the gap
  (RFC 5545 / Temporal "compatible"), overlap = earlier instant once,
  days that do not exist are skipped, `until_ms` inclusive, search
  bounded to 401 years. `java.util.Calendar` resolves overlaps to the
  later (standard-time) instant by default: the Kotlin port must pick
  the earlier one explicitly.
- **Trigger timing** (`Trigger::next_after(anchor, after)` in
  `src/schedule.rs`): `After(d)` = anchor + d once, `At(t)` once
  (`first_fire` delivers a past `At` at once), `Every(p)` = anchor + k*p
  on its grid (missed periods skipped, not shifted), `Calendar` = rule
  occurrences after the anchor.
- **Missed deliveries** (`schedule/engine.rs`): more than
  `LATE_AFTER_MS` (60 s) late counts as missed; lateness is measured from
  the latest missed occurrence (a daily reminder missed for three days is
  as late as today's); `Drop` skips, `DeliverLate(max)` delivers once
  with `DeliveredLate` when within `max`; repeats then move to the next
  occurrence after now.
- **Store format** (Android `ScheduleStore`, if it mirrors it): format
  byte `STORE_FORMAT = 1`, then the wire encoding of
  `Vec<(Notification, Trigger, i64 anchor_ms, Option<i64> next_fire_ms)>`.

### `tests/schedule_vectors.tsv`

Tab separated, `#` comments, one header row. Columns: `name`, `zone`
(rule zone or `-`), `local` (the device zone used when `zone` is `-`),
`hour`, `minute`, `weekdays` (`mon,tue,...` or `-`), `day` (or `-`),
`month` (or `-`), `until` (RFC 3339 UTC or `-`), `after` (RFC 3339 UTC),
`count` (occurrences asked, each strictly after the previous),
`expected` (comma-separated RFC 3339 UTC instants, fewer than `count`
when the rule runs out, `none` when there is none). 31 rows: New York,
Berlin, Sydney and Lord Howe (30-minute shift) gaps and overlaps, Samoa's
skipped 2011-12-30, leap days (2100 skipped), the 29th / 31st, weekday
sets, Friday the 13th, `until`, local wall clock. Runner:
`tests/schedule_vectors.rs`.

### Verification

- Unit tests: ids, validation, render, coalescer, rule, trigger, engine,
  store, images, live queue, and the desktop host end to end through a
  runtime with a recording backend, a manual timer and a fake clock
  (show / channel merge / image files, coalescing to the rate, dismissed
  updates, responses + deep links + acks, choices, carried fields, snooze,
  store restart with missed deliveries, host-run timeouts, messages,
  capability overlay, no platform backend, live notifications).
- Goldens: `tests/golden/notifications_*` and the app Kotlin
  `app_notifications_*`. The app Kotlin compiles with `kotlinc 2.3
  -Werror` against the runtime and `runtime/android/ci` stubs;
  `runtime/ios/ci/typecheck-goldens.sh` (swiftc 5.10
  `-warnings-as-errors`) passes with every plugin's Swift in one module;
  `check_swift_namespaces` passes.
- `cargo check --target wasm32-unknown-unknown` passes (fenix stable
  combine with the wasm32 std); clippy 1.100 is clean on the crate.
- Not done: any platform backend, any device run.

### Left for the platform agents

- **istmo-build**: `[app.notifications]` + `[[app.notification_channel]]`
  (`crates/build/src/notifications.rs`), Android raw resource of static
  channels, manifest permissions, Apple entitlements, desktop static
  channels into `DesktopNotificationsBuilder::with_channels`.
- **Android**: `NotificationsBackendImpl`, trampoline activity and
  receivers, `ScheduleStore`, `AlarmManager` re-arming, `CalendarRule.kt`
  and `Coalescer.kt` ports (vectors + coalescer cases), responses
  publishing with the generated `NotificationResponse` codec, group
  summaries, `istmo.toml` gradle / permissions / manifest fragment and
  `android_backend`, `auto_register = true`.
- **Apple**: iOS Swift backend and delegate (`IstmoNotifications.install`),
  categories, communication notifications, top-up of the 60 armed
  requests, `Coalescer.swift`; macOS Rust `Backend` in
  `desktop/macos.rs` (`NotBundled` outside an `.app`).
- **Windows**: `desktop/windows/` `Backend` (toast XML, data-binding
  progress, COM activator, registry registration, scheduled toasts with
  the 14-day window), `DesktopNotifications::install` / `unregister`.
- **Linux**: `desktop/linux/` `Backend` for fdo (capabilities cache,
  `replaces_id`, `value` hint, KDE inline reply, `NotificationClosed` to
  `dismissed`), portal v1 / v2 (exported actions, `ActivateAction`),
  kuiserver progress; app id resolution.
- **Web**: `src/web.rs` host, a `setTimeout` `schedule::Wakeup`,
  IndexedDB store, the service worker asset.

### Review fixes (core)

Fixes of the adversarial review of the core, before the platform
branches merge. Supersede anything above they contradict.

- **Per-id lanes.** Every backend call for one id (`show`, `remove`)
  runs inside that id's lane (`State.lanes` + a condvar): one at a time,
  in the order issued; the post is prepared from the state the previous
  call left. A cancel waits for the post in flight and lands after it
  (cancel wins). Scheduled deliveries taken from the engine carry a
  ticket (`State.delivering`); a cancel in between revokes it, so a due
  entry cancelled before its show is never shown.
- **Images.** A full `show` (and `LiveNotification::finish`) always
  collects fresh images; the replaced record's files are released.
  Updates reuse the shown files unless they touched icon, style,
  actions or message images. Each post owns its file uses (a reused set
  is retained), so no release is doubled. Image IO and rendering run
  outside the state lock.
- **Reports.** `BackendSink` methods only queue; one report worker
  thread (`istmo-notifications-reports`, started on the first report)
  handles them in order. Sink calls from inside the start function or a
  backend method never re-enter the host, and a snooze's `remove` runs
  on the worker, never on the backend's dispatch thread.
  `DesktopNotifications::settle()` (new) waits until earlier reports are
  handled (tests, backends that need the effect).
- **Load.** Every host call waits until the store is loaded (`OnceLock`);
  `persist()` writes nothing before the load. The backend's rate is
  applied as soon as its info is first read.
- **Backend info.** Cached (`HostInfo`: info, limits with labels,
  overlaid capabilities); re-read only after `changed()`, which also
  applies a new rate at once (`Coalescer::set_rate`, queue kept).
- **Hot path.** `Live.notification` is an `Arc`; `set_progress` checks
  only the bar (`Progress::validate`, new) and copies nothing. `update`
  still validates the whole patched notification (call actions are
  checked there: `NotificationPatch::validate` only refuses them when the
  patch sets a non-call style). Image bytes stay inline (no wire change).
- **Engine.** Entries are `Arc`ed; `persist` snapshots them under the
  lock and encodes outside (borrowed tuples, same bytes). Triggers that
  never fire are refused by `schedule` (`Invalid`, field
  `trigger.until_ms` when the rule has an end, else `trigger`,
  `OutOfRange`; also the fake host), never stored (`Engine::add` keeps
  nothing, `with_entries` drops them). Lateness: a backward doubling
  search finds the latest occurrence before the forward walk (which stays
  capped). The device zone is re-read at each timer wake (≤ 30 s) and
  each tick; floating rules move to their next occurrence in a new zone
  (`Engine::set_local`).
- **Coalescer.** Debt floored at `-burst` tokens; `set_rate` keeps the
  queue. Ports (Kotlin `Coalescer.kt`, Swift `Coalescer.swift`) must
  mirror both; cases `debt_has_a_floor_of_one_burst`,
  `a_new_rate_keeps_the_queue`.
- **Acks.** Tokens are a keyed hash (per-host `RandomState`) of a
  counter, at most 256 kept (oldest dropped). The coalescer's dismissed
  set is bounded by the host table (256).
- **Responses.** `deep_link` only for `Open` and `Button`; native ports
  publishing deep links must do the same.
- **Live.** `LiveNotification::progress` keeps the bar's label, segments
  and points as last shown, set or patched.
- **Store.** File `0600` on Unix, directory synced after rename/removal.

Contract changes for backend agents: `info()` is no longer called per
post; `show`/`remove` for one id never overlap; sink reports are
asynchronous (tests that report and then assert call `settle()`).
API additions: `DesktopNotifications::settle`, `Progress::validate`,
`Coalescer::{set_rate, dismissed_len}`, `Engine::{merge, local,
set_local, snapshot}`. `Engine::add` no longer stores dead entries.

## Implementation notes (istmo-build, as built)

Branch `notif-build`, step 2 of section 8: `crates/build/src/notifications.rs`
(`istmo_build::notifications`), wired into `emit_app`. Everything below
is the contract the platform agents build against; names are constants
in that module.

### Parsing (`AppConfig.notifications: AppNotifications`)

- `[app.notifications]` keys: `small_icon` (`.xml` / `.png`), `accent`
  (`#RRGGBB` = opaque, `#AARRGGBB`), `sounds` (array of paths),
  `exact_alarms` (`none` default / `schedule` / `use`), `full_screen`,
  `live_updates` (default false), `apple = { time_sensitive,
  communication, critical }` (default false), `web_service_worker`
  (default **true**). Unknown keys fail, also inside `apple`.
- Sound name = file stem, `[a-z][a-z0-9_]*`, <= 64 (an Android resource
  name). Extensions: `wav ogg mp3` play on Android, `wav caf aif aiff` on
  Apple; per name at most one Android-playable and one Apple-playable
  file (`ding.caf` + `ding.ogg` is fine, `ding.wav` + `ding.ogg` is not).
  Duration is not checked (needs decoding).
- `[[app.notification_channel]]` (or one `[app.notification_channel]`
  table), keys = `ChannelSpec` fields: `id` and `name` required;
  `importance` `passive|normal|high|urgent|critical` (default `normal`);
  `sound` `default|silent|<name in sounds>` (system names are not
  accepted for static channels); `vibration` <= 32 u32;
  `light_color` like `accent`; `show_badge` (default **true**);
  `lock_screen` `public|hide_content|hidden`; `bypass_dnd` (default
  false); `group` non-blank <= 128 chars. Limits mirror `ids.rs` /
  `ChannelSpec::validate` (the build also refuses a blank `group`, which
  the plugin allows). Duplicate ids fail.
- `StaticChannel` mirrors `ChannelSpec` field for field with wincode
  derives (`id: String` encodes like `ChannelId { value }`; enum variant
  order = wire tags). `plugins/notifications/tests/static_channels.rs`
  decodes istmo-build's bytes as `Vec<ChannelSpec>`; keep both in step
  when `ChannelSpec` changes.

### Android (written into `android/.istmo/res` when compiling for Android)

| Resource | Content |
|---|---|
| `drawable/istmo_notification_icon.xml` | `small_icon` vector, copied |
| `drawable-{mdpi,hdpi,xhdpi,xxhdpi,xxxhdpi}/istmo_notification_icon.png` | `small_icon` PNG as white + source alpha, 24/36/48/72/96 px; a PNG with no transparent pixel fails the build |
| (fallback) | no `small_icon`: `[app.icon.android] monochrome` (PNG: central 2/3 cropped; XML: copied as is) |
| `values/istmo_notifications.xml` | `color/istmo_notification_accent` (`#AARRGGBB`, only when `accent`), `bool/istmo_notifications_full_screen`, `bool/istmo_notifications_live_updates`, `string/istmo_notifications_exact_alarms` (`none`/`schedule`/`use`) |
| `raw/istmo_notification_channels.bin` | wire bytes of `Vec<ChannelSpec>` (no header, istmo codec = bincode 2 standard), only when channels exist |
| `raw/istmo_notification_sound_<name>.<ext>` | Android-playable sounds; `Sound::Named("ding")` = `R.raw.istmo_notification_sound_ding`, URI `android.resource://<pkg>/raw/istmo_notification_sound_ding` |

- The Kotlin backend resolves these with `resources.getIdentifier(name,
  type, context.packageName)` (the R class is the app's, not the
  plugin's); 0 = not configured (no small icon: fall back to the app
  icon; no channels file: no static channels). No manifest meta-data:
  the resources need no `AndroidManifest.xml` markers.
- Read the channels with the generated codec: varint count, then
  `readChannelSpec` per entry. Create them in `NotificationManagerCompat`
  at backend start (create is idempotent; existing ones keep their
  settings).
- Stale `istmo_notification*` files under `.istmo/res` are removed when
  the configuration changes. `istmo.json` `app.generated_res` is true
  whenever either table exists: **no Gradle plugin change**.
- Permissions (sidecar `android/.istmo/istmo-permissions.xml`), whenever
  either table exists: `POST_NOTIFICATIONS`, `RECEIVE_BOOT_COMPLETED`;
  `exact_alarms = "schedule"` -> `SCHEDULE_EXACT_ALARM`; `"use"` ->
  `USE_EXACT_ALARM` + `SCHEDULE_EXACT_ALARM maxSdkVersion=32`;
  `full_screen` -> `USE_FULL_SCREEN_INTENT`; `live_updates` ->
  `POST_PROMOTED_NOTIFICATIONS`. The Android agent still declares the
  base two (plus receivers, trampoline) in the plugin's own manifest so
  apps without the tables work; the sidecar deduplicates.

### Apple (iOS project generator; macOS is packaging's job)

- Merged as app keys (`istmo.toml [app]` owner) into the `istmo:plugins`
  blocks of `<App>/Info.plist` and `<App>/*.entitlements` (sidecars
  `Info.plist.plugins.xml` / `*.entitlements.plugins.xml`); arrays
  concatenate with plugin fragments.
- `Info.plist`, whenever `[app.notifications]` exists:
  `IstmoNotifications` = dict `{ TimeSensitive: bool, Communication:
  bool, Critical: bool, Sounds: { <name>: "<bundle file>" } }`; with
  `communication`: `NSUserActivityTypes` += `INSendMessageIntent`,
  `INStartCallIntent`. The Swift backend reads
  `Bundle.main.object(forInfoDictionaryKey: "IstmoNotifications")`:
  `Critical` false -> map `Interruption::Critical` to time sensitive,
  `TimeSensitive` false -> `Urgent` to active; `Sounds[name]` is the
  `UNNotificationSoundName` (an unknown name: treat as a system / bundle
  name as given).
- Entitlements: `com.apple.developer.usernotifications.time-sensitive`,
  `...communication`, `...critical-alerts` = true for the set flags.
- Sounds: Apple-playable files copied to
  `ios/.istmo/NotificationSounds/istmo_notification_sound_<name>.<ext>`
  and listed in `istmo-plugins.yml` as a `type: group`,
  `buildPhase: resources` source: they land in the bundle root.
  Hand-made Xcode projects add that directory's files to Copy Bundle
  Resources.
- The keys and sounds go away when the table is removed; an emptied
  merge now also clears the `istmo:plugins` block (it used to keep the
  last keys).

### Desktop and web (rustc envs of the app crate, non-mobile targets)

| Env | Value |
|---|---|
| `ISTMO_NOTIFICATION_CHANNELS` | upper-case hex of the same wire bytes; only with channels |
| `ISTMO_NOTIFICATION_ACCENT` | `AARRGGBB` (8 hex digits, no `#`), only with `accent` (Windows `IconBackgroundColor`) |
| `ISTMO_NOTIFICATION_APPLE` | comma list of the set flags (`time_sensitive,communication,critical`), only when one is set (macOS backend) |
| `ISTMO_NOTIFICATION_SOUND_DIR` | `OUT_DIR/istmo-notification-sounds`, every sound as `<name>.<ext>` (all formats; the backend picks: Linux `ogg`/`wav` for the fdo `sound-file` hint, macOS `caf`/`aiff`/`wav`; packaged apps ship their own copy) |
| `ISTMO_NOTIFICATION_SERVICE_WORKER` | wasm32 with `web_service_worker`: `OUT_DIR/istmo-notifications-web/istmo-notifications-sw.js` |
| `ISTMO_APP_NAME` | `[app] name` (else the package), every app (Windows AUMID `DisplayName`); `ISTMO_APP_BUNDLE_ID` and `ISTMO_APP_ICON_PNG` (`istmo::app_icon!()`) already existed |

- Plugin side (this branch): `istmo_notifications::static_channels!()`
  expands in the app to `channels_from_build(option_env!(
  "ISTMO_NOTIFICATION_CHANNELS"))` (unreadable value: warn, no
  channels); `DesktopNotifications::platform()` is the builder
  `Default` uses, so apps write
  `DesktopNotifications::platform().with_channels(static_channels!()).build()`.
  Backends that want the other envs read them the same way (a macro in
  the plugin crate, expanded in the app); none is wired yet.
- Web: the plugin must hand its worker over as an asset:
  `[[assets]] path = "assets/web/istmo-notifications-sw.js"` in
  `plugins/notifications/istmo.toml` (the web agent adds it with the
  file). istmo-build finds it in the `DEP_*_ISTMO_ASSETS` payload of
  `istmo.notifications` by file name and copies it; with the plugin
  linked but no such asset it warns. Serving it at the site root is the
  app's bundler step (the env names the file).

### Not done / open

- No device or Xcode / Gradle run; the outputs are checked by unit and
  integration tests (`crates/build/src/notifications/tests.rs`,
  `crates/build/tests/notifications.rs`, goldens
  `crates/build/tests/golden/notifications_{values,info_plist,entitlements}.xml.expected`).
- Sound length (30 s Apple limit) and Android vector validity are not
  checked. A monochrome XML fallback keeps its 108 dp viewport (glyph
  looks small).
- `istmo doctor` does not report missing notification entitlements in
  provisioning profiles.
