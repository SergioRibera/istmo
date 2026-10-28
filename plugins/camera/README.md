# istmo-camera

Cross-platform camera plugin for [istmo](https://docs.rs/istmo).

- Enumerate every camera the OS exposes with normalised capability
  flags (facing, lens kind, zoom range, OIS/EIS, flash, HDR, depth,
  RAW, resolutions).
- Stream raw frames in real time through a typed
  `TypedStream<CameraFrame>`. Native-side ring buffer absorbs
  consumer backpressure and reports dropped frames per delivery.
- Drive zoom, torch, exposure compensation, focus and white balance
  on a running session.
- Open multiple sessions concurrently on supported devices (iOS A12+
  `AVCaptureMultiCamSession`, Android ≥ 11 CameraX concurrent camera).
- Deterministic native cleanup via `NativeHandle<CameraSessionTag>` —
  dropping the session releases the pipeline.

## Install

```toml
[dependencies]
istmo = { version = "0.1", features = ["camera"] } # or:
istmo-camera = "0.1"
```

Add the plugin to your runtime:

```rust
istmo::runtime! {
    plugins: [
        istmo_camera::CameraClient,
    ],
}
```

With `istmo-build` auto-wiring (`emit_wiring_env` in your app's
`build.rs`), the plugin is picked up by `istmo.toml` discovery and
you don't need to list it manually.

## Permissions

- Android: `android.permission.CAMERA` (declared via plugin).
- iOS: `NSCameraUsageDescription` in Info.plist. The plugin ships a
  default string; override it from your app's `[app.info_plist]`
  table in `istmo.toml` to use your own copy.

Prompt the user from Rust:

```rust
match camera.request_access().await? {
    AccessStatus::Granted => { /* open sessions */ }
    AccessStatus::Denied | AccessStatus::Restricted => {
        // deep-link to Settings, surface rationale, etc.
    }
    AccessStatus::NotDetermined => unreachable!("prompt resolves"),
}
```

## Minimal capture loop

```rust
use std::sync::Arc;
use istmo_camera::{
    AccessStatus, CameraClient, CameraClientExt, CameraInitConfig, CameraFacing,
    PixelFormat, Resolution, SessionConfig,
};
use istmo_core::StreamItem;

async fn capture() -> Result<(), Box<dyn std::error::Error>> {
    let cam = Arc::new(CameraClient::acquire_with(CameraInitConfig::default()).await?);

    if cam.request_access().await? != AccessStatus::Granted {
        return Err("camera access denied".into());
    }

    let devices = cam.devices().await?;
    let back = devices
        .iter()
        .find(|d| d.facing == CameraFacing::Back)
        .ok_or("no back camera")?;

    let session = cam
        .open_session(
            SessionConfig::new(&back.id)
                .with_pixel_format(PixelFormat::Nv21)
                .with_resolution(Resolution { width: 1280, height: 720 })
                .with_target_fps(30),
        )
        .await?;

    let frames = session.frames()?;
    loop {
        match frames.recv_async().await? {
            StreamItem::Event(frame) => {
                if frame.dropped_before > 0 {
                    log::warn!("dropped {} frames", frame.dropped_before);
                }
                // frame.bytes is YUV_420 planar; frame.planes describes
                // the layout. Hand to a decoder, GPU uploader, ML
                // inference pass, …
            }
            StreamItem::Completed | StreamItem::Cancelled => break,
            StreamItem::Failed(err) => return Err(Box::new(err)),
        }
    }

    session.close().await?;
    Ok(())
}
```

Dropping the `SessionHandle` releases the native session; the
explicit `close().await` is only useful when you want to observe the
backend's close error.

## Multiple cameras at once

```rust
let back = cam.open_session(SessionConfig::new(back_id)).await?;
let front = cam.open_session(SessionConfig::new(front_id)).await?;

let (bf, ff) = (back.frames()?, front.frames()?);
// Interleave both streams in your own task.
```

Check `CameraClient::concurrent_support().await?` ahead of time:

- `max_concurrent == 1` → fail over to switching between sessions
  instead of opening both.
- `approved_pairs` → only the listed `(primary, secondary)` tuples
  are safe on Android. Open any other pair at your own risk.
- `any_combination` → iOS multi-cam, pair any two visible cameras.

## Controls

```rust
use istmo_camera::{CameraControls, FlashMode, FocusMode, WhiteBalanceMode};

let applied = session
    .apply_controls(CameraControls {
        zoom: Some(2.0),
        torch: Some(true),
        focus: Some(FocusMode::ContinuousVideo),
        exposure_compensation_ev: Some(-0.3),
        white_balance: Some(WhiteBalanceMode::Daylight),
        flash: Some(FlashMode::Off),
    })
    .await?;
```

`AppliedControls` reports the effective state — the backend clamps
values that fall outside what the hardware supports.

## Pixel formats and backpressure

Streaming raw frames to Rust is a memcpy per plane per frame. Pick a
pixel format matched to your consumer:

- `PixelFormat::Nv21` / `PixelFormat::Yuv420` — cheapest native path,
  ~1.5 bytes per pixel. Good for GPU uploads and ML models that
  accept YUV.
- `PixelFormat::Bgra8` / `PixelFormat::Rgba8` — 4 bytes per pixel.
  Convenient but ~2.5× the bandwidth of YUV.
- `PixelFormat::Jpeg(quality)` — hardware-encoded (where available).
  Order-of-magnitude cheaper over the wire but adds encode/decode
  latency.

The native backend keeps a ring buffer sized by
`CameraInitConfig::frame_pool_depth` (default `3`). When a consumer
falls behind, the oldest frame is dropped. Each delivered frame
reports `dropped_before` so Rust-side code can log, downsample, or
back off.

For high-resolution preview-only pipelines, open the session with
`SessionConfig::preview_only()` — the stream stays silent and no
copy fires per frame. (The native widget rendering path is the
app's responsibility.)

## Desktop (Linux, macOS, Windows)

Pure-Rust host behind the `desktop` feature:

```toml
[dependencies]
istmo-camera = { version = "0.1", features = ["desktop"] }
```

Backed by [`nokhwa`](https://docs.rs/nokhwa) — V4L2 on Linux,
AVFoundation on macOS, Media Foundation on Windows. Supports
`PixelFormat::Rgba8` and `PixelFormat::Bgra8` (nokhwa decodes the
camera's native YUYV / NV12 / RAWRGB stream into packed 32-bit).
Other formats return `CameraError::FormatUnsupported` on `open()`.

Register the host with the runtime:

```rust
use istmo::Runtime;
use istmo_camera::{CameraHost, DesktopCameraFactory};

let runtime = Runtime::init(/* ... */)?;
runtime.register_host(CameraHost::new(DesktopCameraFactory));
```

NixOS builds pull the backend's C deps through `nix develop` —
`v4l-utils`, `libclang`, `gcc` and `nasm` are already in the
project's `flake.nix` devshell.

## Web (wasm32)

Scaffold shipped behind the `web` feature. The surface matches
desktop + mobile; the backend is a stub until the real
`web-sys::MediaDevices` pipeline lands. See `src/web.rs` for the
roadmap (MediaStreamTrackProcessor vs. canvas + rAF, constraint
API, permission mapping).

```toml
[target.'cfg(target_arch = "wasm32")'.dependencies]
istmo-camera = { version = "0.1", features = ["web"] }
```

## Native layer

Reference backends live under `native/{android,ios}/`:

- `CameraBackendImpl.kt` — CameraX pipeline with `ImageAnalysis`
  (`STRATEGY_KEEP_ONLY_LATEST`) feeding a per-session
  `MutableSharedFlow`. `Camera2CameraInfo` interop pulls full
  capability metadata.
- `CameraBackendImpl.swift` — `AVCaptureSession` (or
  `AVCaptureMultiCamSession` when available) with
  `AVCaptureVideoDataOutput`. Sample buffers extracted into
  `AsyncStream<CameraFrame>`.

Both implement `HandleReleaser` so `NativeHandle::drop` on the Rust
side tears the session down deterministically.

## Still capture

`SessionConfig` and the trait include a `capture_still` entry point.
The reference backends throw `CameraError::Backend(…)` by default —
wire an `ImageCapture` (Android) or `AVCapturePhotoOutput` (iOS)
use case into the session's `open()` if your app needs stills.

## Design notes

- **No `cfg(target_os = …)` in app code.** Capability flags come as
  bool fields on `CameraCapabilities`; gate features on them, not on
  the target OS.
- **Session lifetime = native lifetime.** The owned `SessionHandle`
  carries a `NativeHandle<CameraSessionTag>`; drop it to release.
- **Reusable session controls.** `CameraControls` fields are all
  `Option`; set only the knobs that change. Default = "leave this
  alone".
- **Backpressure is observable, not hidden.** `dropped_before` on
  each frame tells the Rust consumer when it's falling behind so it
  can downgrade on its own terms.
