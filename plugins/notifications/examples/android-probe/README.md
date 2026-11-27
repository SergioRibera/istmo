# notifications-probe

An Android app that exercises `istmo-notifications`' Kotlin backend on a
device. Each button (or an intent extra) runs one check in Rust and
prints a report; reports and every response (tap, button, reply, with
its ack completed after 500 ms) are logged under the `notif-probe` tag.

```sh
nix develop ../../../..#bench-android
cd android
gradle -Pandroid.aapt2FromMavenOverride=$ANDROID_HOME/build-tools/35.0.0/aapt2 :app:installDebug
adb shell am start -n dev.istmo.notificationsprobe/.ProbeActivity --es cmd show
adb logcat -s notif-probe
```

| Command | Checks |
| --- | --- |
| `caps`, `perm`, `ask` | capability report, permission state, the `POST_NOTIFICATIONS` prompt |
| `show` | large icon from PNG bytes, tap: `Open` response and deep link |
| `styles`, `chat`, `call` | big text, inbox, picture; conversation plus an appended message; simulated call |
| `actions` | five actions (two plus More), background button, choices, snooze, replies |
| `progress [n]`, `segments` | live progress coalesced to 4 per second and finish; `ProgressStyle` with segments and points |
| `group` | three grouped notifications and the plugin's summary |
| `group-shrink` | two grouped, one cancelled: the other survives (the summary is kept) |
| `limit` | distinct ids until `TooMany(50)`, then removed |
| `invalid` | unchecked `show` / `schedule` / `complete`: `Invalid(title)`, `Invalid(trigger.every)`, `NotFound` |
| `schedule [s]`, `pending` | a one-shot after `s` seconds and an every-minute repeat |
| `delivered`, `events [s]`, `cancel-all` | active list, the event stream for `s` seconds, cleanup |
