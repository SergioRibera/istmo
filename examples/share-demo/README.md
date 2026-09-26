# share-demo

One Rust UI (egui) that exercises every part of `istmo-share` on a real
device: sending, rich previews, cancelling an open sheet, direct-share
targets and receiving content from other apps. The same code runs on
desktop (`cargo run -p share-demo`).

## Android

Needs a device with USB debugging (`adb devices` lists it) and Docker —
the build runs inside the same image as the other demos.

```sh
just share-demo          # build + install + launch
# or step by step
just build share-demo
just install share-demo
just run share-demo
```

The APK lands in `examples/share-demo/android/app/build/outputs/apk/debug/`.
Logs: `adb logcat -s share_demo istmo RustStdoutStderr`.

### What to try

| Area | Steps | Expected |
| ---- | ----- | -------- |
| Text / link | *share* with the defaults | chooser opens; transcript shows `TargetChosen(<component>)` after picking an app, `Unknown` after backing out |
| Files | tick *attach note.txt* and/or *attach image.png*, *share* | the target receives a readable `.txt` / `.png` (Gmail, Drive, Messages…) |
| Mixed | text + link + both files | targets that accept `*/*` get everything |
| Rich preview | tick *rich preview*, *share* | Android 10+: title and gradient thumbnail at the top of the chooser |
| Cancel | *share, cancel after 3 s* | after 3 s the transcript shows `cancelled`; the chooser stays open (Android cannot close it) |
| Capabilities | top of the screen | `receive` and `direct share` are ✔ because the manifest declares the alias |
| Receive, warm | open Photos/Chrome/Files → share → *istmo share demo* | the demo comes to front; the share appears under *received* with its files readable |
| Receive, cold | swipe the demo away, then share into it | same, delivered from the pre-start buffer |
| Direct share | *publish targets*, then share from another app | *Family chat* / *Work notes* appear in the chooser's suggestion row (can take a few shares to rank); picking one shows `to target "family"` |
| Cleanup | *delete received files* | transcript shows `files deleted` |

## Desktop

```sh
cargo run -p share-demo
```

Windows and macOS open the system share UI anchored on the demo window;
Linux reports every capability as ✘ (no system share sheet).
