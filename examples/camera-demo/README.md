# camera-demo

Single eframe + egui app that runs the `istmo-camera` plugin on:

- **Desktop** — Linux / macOS / Windows. Pure Rust (`DesktopCamera`
  via `nokhwa`).
- **Browser** — wasm32-unknown-unknown. Pure Rust (`WebCamera` via
  `web-sys` + `getUserMedia`).

Mobile targets ship the same `app.rs`; wiring the Android / iOS
scaffold mirrors `examples/rust-mobile-demo` and is deferred for now.

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
