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
that are still running. An activity whose stored payload no longer
decodes is skipped with a warning and released.

Without a registered native backend (e.g. on desktop) every call fails;
probe `LiveActivityClient::capabilities` first.
