# istmo-live-activity

Live Activities from Rust, for the
[istmo](https://github.com/SergioRibera/istmo) framework.

| Platform | Backend | Minimum |
| --- | --- | --- |
| iOS | ActivityKit / Dynamic Island (`native/ios`) | 16.2 at runtime; older releases build and report no support |
| Android | Ongoing notifications in three tiers: custom `RemoteViews`, `ProgressStyle` and Live Updates (both API 36) (`native/android`) | API 26 |
| Desktop | none — every call fails | — |

The Android minimum comes from the plugin's `[min_versions]` and is
checked by the app's `build.rs`.

## Install

```toml
[dependencies]
istmo-live-activity = "0.1.1-alpha.5"
```

## Usage

```rust,ignore
use istmo_live_activity::{ActivityStyle, DismissalPolicy, TypedLiveActivity};

let timers = TypedLiveActivity::<TimerAttributes, TimerState>::new(&runtime, "timer")?;
let handle = timers.start(attrs, TimerState::default(), ActivityStyle::Standard).await?;
timers.update(&handle, next_state, None).await?;
timers.end(&handle, Some(final_state), DismissalPolicy::Default).await?;
```

Attributes and state are any `#[istmo::message]` types; they travel as
bytes and the native handler registered for the activity type decodes
them. Register one `LiveActivityHandler` per activity type on the
native `LiveActivityBackendImpl`; a type without one fails with
`ActivityError::UnknownActivityType`.

After a restart, `TypedLiveActivity::restore` returns the activities
of its type that are still running. Dropping a handle ends its
activity on both platforms, so `restore` never hands out a second
handle for an activity this process already owns: calling it again
returns only activities not seen before. An activity whose stored
payload no longer decodes is skipped with a warning and left running
(it is not adopted, so nothing ends it). Native handlers must report
an activity that already has a handle under that same handle.

Without a registered native backend (e.g. on desktop) every call fails;
probe `are_activities_enabled` or `capabilities` first.

## App setup

* **iOS**: set `NSSupportsLiveActivities = true` (e.g. through
  `[app.info_plist]` in the app's `istmo.toml`) and add a Widget
  extension that renders your `ActivityAttributes`.
* **Android**: on Android 13+ the app declares `POST_NOTIFICATIONS` and
  requests it at runtime; without it `start` fails with
  `ActivityError::Disabled`.

The handlers are app code, so apps usually set `auto_register = false`
for `istmo.live_activity` under `[[app.plugin]]`, build
`LiveActivityBackendImpl` themselves, `register` their handlers on it
and register it with `LiveActivityDispatcher` (see
`examples/live-activity-demo`).

Full guide: <https://sergioribera.rs/istmo/plugins/live-activity/>
