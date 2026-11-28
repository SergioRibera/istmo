# istmo-notifications

Local notifications for the [istmo](https://github.com/SergioRibera/istmo)
framework on Android, iOS, macOS, Windows, Linux and the web, with live
updates. One model everywhere: a platform feature is used when it
exists, simulated when it does not (and `capabilities()` says which),
refused only when there is no honest way to do it.

## Three examples

A chat message with an inline reply:

```rust,ignore
use istmo_notifications::{Action, ChatMessage, Conversation, Notification, Participant, Style};

let ana = Participant::new("ana", "Ana");
let chat = Notification::new("chat:42", "Ana")?
    .with_style(Style::Messaging(
        Conversation::new(Participant::new("me", "Me"))
            .with_message(ChatMessage::received(ana, "Lunch at one?")),
    ))
    .with_action(Action::reply("reply", "Reply")?)
    .with_action(Action::button("read", "Mark read")?.background());
client.show_checked(chat).await?;
```

A download with live progress:

```rust,ignore
use istmo_notifications::{Action, Notification, Progress};

let live = client
    .show_live(
        Notification::new("dl:42", "Downloading report.pdf")?
            .with_progress(Progress::fraction(0.0).with_label("report.pdf"))
            .with_action(Action::button("cancel", "Cancel")?.destructive())
            .ongoing(),
    )
    .await?;
for chunk in 1..=100 {
    live.progress(chunk as f32 / 100.0); // never waits; coalesced to the platform's rate
}
live.finish(Notification::new("dl:42", "Download complete")?.with_url("app://files/42"))
    .await?;
```

A daily reminder:

```rust,ignore
use istmo_notifications::{Notification, Trigger};

client
    .schedule_checked(
        Notification::new("water", "Drink some water")?,
        Trigger::daily(9, 30),
    )
    .await?;
```

## Responses, also at cold start

What the user does (a tap, a button, a reply) is published on the
`istmo.notifications.responses` queue. Responses that arrive before the
app subscribes (the tap that launched it) wait in the queue:

```rust,ignore
use istmo_notifications::{NotificationResponses, Response};

let responses = NotificationResponses::acquire()?.stream();
while let Ok(r) = responses.recv_async().await {
    match r.kind() {
        Response::Reply(text) => chat.send(&r.id, text).await,
        Response::Action("read") => chat.mark_read(&r.id).await,
        Response::Open => router.open(&r.id),
        _ => {}
    }
    if let Some(ack) = r.ack {
        client.complete(ack).await?; // background action handled
    }
}
```

A response with a URL is also published as a deep link on
`istmo.deeplinks` (`source = "notification:<id>"` or
`"notification:<id>:<action>"`), so apps that route links get
notification taps for free.

## Install

```toml
[dependencies]
istmo-notifications = "0.1.1-alpha.7"
```

Register the host for the platform: `NotificationsHost::new(DesktopNotifications::default())`
on Linux, Windows and macOS (keep a clone and call `attach(&runtime)` at
start so missed reminders and launch taps are handled before the first
call), `UnavailableNotifications` where there is no backend.
`FakeNotifications` (feature `testing`) records calls for app tests.

## What you get

| | Android | iOS / macOS | Windows | Linux | Web |
|---|---|---|---|---|---|
| Update in place, silently | native | re-add, quiet | native | native | native |
| Progress bar | native | text line | native | native or text line | text line |
| Inline reply | native | native | native | native where the server has it, else a button | button |
| More actions than shown | "More" action | "More" action | "More" action | "More" action | "More" action |
| Schedule and repeat | native | native (iOS), while running (macOS) | native | while running | while running |
| Snooze | simulated | simulated (iOS), while running (macOS) | native | while running | while running |
| Timeout | native (8.0+) | while running | native | while running | while running |
| Taps when closed | native | native | native | portal only | service worker |

`capabilities()` reports every feature as `Native`, `Simulated`,
`WhileRunning` or `Unsupported` for the running backend.

## Platform notes

- **macOS needs an `.app` bundle.** `UNUserNotificationCenter` only works
  from a bundled, signed app with a bundle id; an unbundled binary gets
  `NotificationError::NotBundled`.
- **Linux without the portal loses taps while closed.** The
  freedesktop notification server never starts the app; only the
  portal (Flatpak, Snap, registered host apps) can, through exported
  actions: package a D-Bus `.service` file for the app id and set
  `DBusActivatable=true`. `desktop::linux::LinuxOptions` sets the app id
  (default: `FLATPAK_ID`, else the executable's name) or forces the
  server or the portal. Progress is a bar on dunst, mako, swaync and
  xfce4-notifyd, a job view on KDE Plasma, text elsewhere.
- **Web scheduling only runs while the page is open.** Browsers have no
  notification scheduler.
- **Android small icons must be monochrome.** Use a vector or an
  alpha-only PNG (`[app.notifications] small_icon`); coloured icons show
  as a white square.
- **Exact alarms are a Play policy decision.** `USE_EXACT_ALARM` is
  reserved for alarm and calendar apps; others ask for
  `SCHEDULE_EXACT_ALARM` and fall back to inexact delivery
  (`ScheduledInfo::exact`).
- **Android registers itself.** The generated plugin registry builds the
  Kotlin backend with the application context; its tap activity and
  receivers come from the plugin manifest. Background actions started
  while the app process was not running reach the app at its next start,
  without an ack.

## Web

`WebNotifications` (on `wasm32`) shows through the plugin's service
worker, or through page notifications when there is none (no buttons,
no taps while the page is closed, nothing at all on Chrome for Android).
To set it up:

1. Serve `istmo-notifications-sw.js` next to the app's page, same
   origin, as `text/javascript`. A wasm32 build of the app (`emit()` in
   its `build.rs`) stages it at `.istmo/web/istmo-notifications-sw.js`;
   copy it into the output, e.g. a trunk `post_build` hook running
   `cp .istmo/web/istmo-notifications-sw.js "$TRUNK_STAGING_DIR/"`, or
   after `wasm-pack build`. It is also `web/istmo-notifications-sw.js`
   in this crate and `istmo_notifications::web_assets::SERVICE_WORKER_JS`.
   If it cannot be registered the host logs why and uses page
   notifications at once.
2. Register `NotificationsHost::new(WebNotifications::default())`; keep a
   clone and call `attach(&runtime)` at start, so taps that opened the
   page and missed reminders are handled before the first call.
3. With your own service worker, add
   `importScripts("istmo-notifications-sw.js");` to it and use
   `WebNotifications::builder().service_worker(ServiceWorker::Existing)`.
4. Call `request_permission` from a click handler (Firefox and Safari
   only prompt during a user gesture).

Taps are queued in IndexedDB by the worker and drained by the window it
focused or opened, so each reaches one tab, also when it opened the app.
Scheduled notifications are kept in IndexedDB, one record each, and
fire while a page is open; with several tabs one runs the schedule (Web
Locks, else a `BroadcastChannel` lease) and the others hand it their
changes.

## Windows

Toasts need a registration and, for taps on a closed app, a COM
activator. Call `install` first thing in `main`, before the runtime
starts (COM waits at most 60 s for the activator when a toast started
the process):

```rust,ignore
fn main() {
    #[cfg(windows)]
    let _ = istmo_notifications::DesktopNotifications::install(
        &istmo_notifications::windows_app!().with_icon_png(istmo::app_icon!().unwrap_or_default()),
    );
    // DocumentsPublisher, runtime, hosts ...
}
```

- `windows_app!()` takes `[app] id` as the AUMID (at most 128
  characters, no spaces), `[app] name` as the display name and
  `[app.notifications] accent` as the icon background.
- Unpackaged apps: `install` writes
  `HKCU\Software\Classes\AppUserModelId\<AUMID>` (`DisplayName`,
  `IconUri`, `IconBackgroundColor`, `CustomActivator`) and
  `HKCU\Software\Classes\CLSID\{CLSID}\LocalServer32` =
  `"<exe>" -istmo-notification` on every start (the exe may move), sets
  the process AUMID, and registers the activator. No Start-menu shortcut.
  The CLSID is a UUID v5 of the AUMID (`WindowsApp::activator_clsid`).
- Packaged (MSIX) apps write nothing: the manifest declares
  `com:ExeServer` with that CLSID and `Arguments="-istmo-notification"`,
  plus `desktop:ToastNotificationActivation`.
- When COM starts the app for a toast, the command line carries
  `-istmo-notification -Embedding`: `launched_by_notification()` says so,
  `app_args()` gives the arguments without them. The response arrives on
  the responses lane with `launched_app = true` once the host is attached.
  A console-subsystem exe detaches from the console COM gave it.
- Single-instance apps: every instance calls `install` first; the one
  that finds a primary running and forwards to it calls
  `DesktopNotifications::revoke_activator()` as soon as it knows, so COM
  routes later taps to the primary. An activation that already reached
  the forwarding instance (COM started it during the primary's startup)
  arrives on its responses lane: forward it with the rest of the launch.
- `cancel` and `cancel_all` also remove toasts an earlier process showed
  (by tag, and the whole `istmo` toast group for `cancel_all`).

- `DesktopNotifications::unregister(&app)` removes toasts, scheduled
  toasts and both registry keys (uninstallers).
- Limits: elevated processes are refused (`Elevated`), builds before
  17763 (1809) answer `Unsupported`. Images are local files only (bytes
  are written to the cache). Sounds: only `ms-winsoundevent:` names
  (`Sound::Named("ms-winsoundevent:Notification.Reminder")`); bundled
  sounds play the default one. At most 5 buttons and 5 inputs; the
  toast XML at most 5 KB (carried user data is dropped from buttons
  first). `urgent` needs build 22546; alarms and reminders need a
  button.
- Progress updates use data binding (`ToastNotifier.Update`): no popup,
  the toast keeps its place. Scheduled deliveries within 14 days (at
  most 50) are also armed as scheduled toasts, so they appear while the
  app is closed.

## Apple

### macOS: bundle required

`UNUserNotificationCenter` works only inside a bundled app: the
executable must run from `<Name>.app/Contents/MacOS/` and the bundle's
`Info.plist` must carry a `CFBundleIdentifier`. Run from `cargo run`, a
bare binary or a `target/` directory, every call answers
`NotificationError::NotBundled` (logged once at warn) and
`capabilities()` reports nothing native. Bundle the app (for example
with `cargo-bundle` or `nix-bundle-app`) and sign it (ad hoc is enough
locally) to see notifications.

- Call `DesktopNotifications::attach(&runtime)` before the event loop
  starts: the delegate must be set before launch finishes, or the tap
  that launched the app is lost (the backend warns when it starts late).
- An app delegate set before the host starts is kept: everything that is
  not the plugin's goes to it. One set after replaces the plugin's, so it
  hands the plugin's callbacks over with
  `istmo_notifications::desktop::macos::forward_will_present` /
  `forward_did_receive` (both `false` for the app's own notifications).
- Scheduling runs in-process on macOS (`WhileRunning`), like the other
  desktop targets; missed ones follow their `MissedPolicy` at the next
  start.
- macOS 12+: interruption levels, relevance, and communication
  notifications when `INSendMessageIntent` is in `NSUserActivityTypes`
  and the app has the communication entitlement. Time sensitive and
  critical need their entitlements, which the backend reads from the
  bundle's `Info.plist` `IstmoNotifications` dictionary (`TimeSensitive`,
  `Critical`: the keys istmo-build writes for iOS; macOS packaging writes
  them): without one a level falls back one step, as on iOS.

### iOS

Zero native code: the generated registry builds `NotificationsBackendImpl`
before `UIApplicationMain`, which sets the `UNUserNotificationCenter`
delegate. Apps with their own delegate forward
`IstmoNotifications.willPresent(_:completion:)` and
`IstmoNotifications.didReceive(_:completion:)`. Inputs are validated like
on desktop (same `Invalid` fields). Scheduling arms one-shot date
triggers at the instants the Rust rules compute (calendar DST gaps,
`Every` on its grid, `until_ms`); the system keeps 64 pending requests
per app, so the plugin keeps every entry and arms the soonest 60
occurrences, topping up at launch, on foreground and in the delegate
callbacks. Occurrences that had no slot while the app was not running
follow their `MissedPolicy` at the next top-up. Background actions hold
the completion handler until `complete(ack)` (at most 25 s).

## Status

The Rust core (types, validation, render fallbacks, coalescing,
scheduling, the desktop host) and the Android, iOS, macOS, Windows,
Linux and web backends are in place. None has run on a device yet.
