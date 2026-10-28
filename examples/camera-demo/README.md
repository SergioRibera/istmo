# camera-demo

Single eframe + egui app that runs the `istmo-camera` plugin on:

- **Desktop** — Linux / macOS / Windows. Pure Rust (`DesktopCamera`
  via `nokhwa`).
- **Browser** — wasm32-unknown-unknown. Pure Rust (`WebCamera` via
  `web-sys` + `getUserMedia`).

- **Android** — eframe + wgpu through `IstmoGameActivity`. Same
  `app.rs`; the generated `CameraDispatcher.kt` + the plugin's
  `CameraFactoryImpl.kt` are auto-wired by `IstmoPluginRegistry`.

## Run on desktop

Inside the project's nix devshell (so V4L2 / libclang / gcc / nasm
are available):

```sh
nix develop
cargo run -p camera-demo --release
```

- First launch triggers the OS permission prompt on macOS (TCC) and
  Windows (privacy settings). Linux has no prompt — the user owns
  the `/dev/video*` nodes.
- Pick a camera in the left panel, press **open**, drag the zoom
  slider, toggle torch (where supported).

## Install on Android

The project's docker image carries the SDK + NDK + Rust Android
targets. From the repo root, one command builds, installs, and
launches on an attached device:

```sh
just camera-demo
```

(Expands to `build camera-demo` → `install camera-demo` → `run camera-demo`,
mirroring `just pen-demo` / `just data-store`.)

Watch logs:

```sh
adb logcat -s camera-demo istmo.camera RustStdoutStderr
```

Without docker: install the Android SDK (API 34), NDK r26 or later,
and `rustup target add aarch64-linux-android`. The istmo gradle
plugin invokes cargo for every ABI listed in
`android/app/build.gradle.kts` → `abiFilters` (default `arm64-v8a`).

The permission prompt fires the first time the app opens a session
(`request_access` on the Rust side → `ActivityResultContracts.RequestPermission`
on the Kotlin side).

## Run in the browser

Install the wasm32 target + `trunk` once:

```sh
rustup target add wasm32-unknown-unknown   # or `nix shell nixpkgs#rustc`
cargo install trunk
```

Then from `examples/camera-demo`:

```sh
trunk serve --release
```

Trunk serves at <http://127.0.0.1:8080> by default. **The browser's
Permissions-Policy requires a secure origin — `getUserMedia` only
fires on `https://` or `http://localhost`.** For mobile testing,
tunnel through `cloudflared`, `localhost.run`, or similar.

On a phone: open the served URL in the mobile browser, grant camera
access when prompted, pick the device.

## What it exercises

| UI element       | Plugin method                                        |
|------------------|------------------------------------------------------|
| `rescan devices` | `CameraClient::devices()`                            |
| `recheck`        | `CameraClient::access_status()`                      |
| `request`        | `CameraClient::request_access()`                     |
| `open` / `close` | `CameraClient::open_session()` / `SessionHandle::close()` |
| zoom slider      | `SessionHandle::apply_controls({ zoom })`            |
| torch toggle     | `SessionHandle::apply_controls({ torch })`           |
| preview          | `SessionHandle::frames()` → `TypedStream<CameraFrame>` |

The frame stream is drained from `eframe::App::update` via
`TypedStream::try_recv` so the same code path works on native (bg
thread optional) and wasm (single-threaded).
