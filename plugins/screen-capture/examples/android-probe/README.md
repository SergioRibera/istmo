# screen-capture-probe

An Android app that exercises `istmo-screen-capture`'s MediaProjection
backend on a device. Each button (or an intent extra) runs one check in
Rust and prints a report, also logged under the `capture-probe` tag.

```sh
nix develop ../../../..#bench-android
cd android
gradle :app:installDebug
adb shell am start -n dev.istmo.screencaptureprobe/.ProbeActivity --es cmd "stream 5"
adb logcat -s capture-probe
```

The consent dialog appears for every capture command; `audio*` commands
play a 440 Hz tone so playback capture has something to record (grant
`RECORD_AUDIO` first, e.g. `adb shell pm grant dev.istmo.screencaptureprobe
android.permission.RECORD_AUDIO`).

| Command | Checks |
| --- | --- |
| `caps` | backends, capability report, sources, access states |
| `stream [s]` | `HardwareBuffer` frames, zero copy: rate, latency, pool identity |
| `stream-any`, `stream-cpu`, `stream-max15`, `stream-exact10` | native tier, CPU tier, pacing |
| `hold` | frames held until the pool stops, resumption on release |
| `shot`, `shot-gpu`, `pixels` | screenshots as frames (CPU, `HardwareBuffer`) and as pixels |
| `preview [s]`, `fanout [s]` | preview into the `SurfaceView`, with and without frames |
| `audio [s]`, `audio-only [s]` | playback audio with video, and alone from a grant |
| `grant` | a grant opens one session; its second use is refused |
| `refuse` | requests refused without showing the dialog |

The crate is its own workspace: it builds only for Android, through its
Gradle project.
