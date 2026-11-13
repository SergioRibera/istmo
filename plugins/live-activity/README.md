# istmo-live-activity

Live Activities from Rust, for the
[istmo](https://github.com/SergioRibera/istmo) framework.

| Platform | Backend |
| --- | --- |
| iOS 16.1+ | ActivityKit / Dynamic Island (`native/ios`) |
| Android | Ongoing notifications in three tiers: custom `RemoteViews`, `ProgressStyle` (API 35), Live Updates (API 36) (`native/android`) |

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
native `LiveActivityBackendImpl`.

After a restart, `TypedLiveActivity::restore` returns the activities
of its type that are still running. Dropping a handle ends its
activity on both platforms, so `restore` never hands out a second
handle for an activity this process already owns: calling it again
returns only activities not seen before. An activity whose stored
payload no longer decodes is skipped with a warning and left running
(it is not adopted, so nothing ends it). Native handlers must report
an activity that already has a handle under that same handle.

`restore_active` takes the activity type, and native backends only
allocate handles for activities of that type (an unregistered type
fails with `UnknownActivityType`). Custom `LiveActivityBackend`
implementations must update `restore_active()` to
`restore_active(activity_type)`.

Without a registered native backend (e.g. on desktop) every call fails;
probe `LiveActivityClient::capabilities` first.
