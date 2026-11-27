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
| Schedule and repeat | native | native | native | while running | while running |
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

## Status

The Rust core (types, validation, render fallbacks, coalescing,
scheduling, the desktop host) is in place. The Android, Apple, Windows,
Linux and web backends are being written; until one lands, its target
answers `Unsupported`.
