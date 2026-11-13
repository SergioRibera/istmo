# istmo-camera

Cross-platform camera plugin for the
[istmo](https://github.com/SergioRibera/istmo) framework.

- Enumerate every camera the OS exposes with normalised capability
  flags (facing, lens kind, zoom range, OIS/EIS, flash, HDR, depth,
  RAW, resolutions).
- Stream raw frames in real time through a typed
  `TypedStream<CameraFrame>`. Each subscriber gets every frame through
  its own ring buffer, which absorbs backpressure and reports dropped
  frames per delivery.
- Drive zoom, torch, exposure compensation, focus and white balance
  on a running session, and capture JPEG stills.
- Open multiple sessions concurrently on supported devices (iOS
  `AVCaptureMultiCamSession`, Android 11+ concurrent camera ids).
- Deterministic native cleanup via `NativeHandle<CameraSessionTag>` —
  dropping the session releases the pipeline.

| Platform | Backend | Minimum |
| --- | --- | --- |
| Android | CameraX (`native/android`) | API 21 |
| iOS | `AVFoundation` (`native/ios`) | 14.0 |
| Linux / macOS / Windows | `DesktopCamera` over `nokhwa` (`desktop` feature) | — |
| Web (wasm32) | `getUserMedia` + canvas (`web` feature) | — |

The mobile minimums come from the plugin's `[min_versions]` and are
checked by the app's `build.rs`.

## Install

```toml
[dependencies]
istmo-camera = "0.1.1-alpha.5"
```

Add the plugin to your runtime:

```rust,ignore
istmo::runtime!(
    plugins: [istmo_camera::CameraClient],
);
```

An app whose `build.rs` runs `istmo_build::emit()` gets the plugin
auto-wired from its `istmo.toml`, so listing it is optional.

## Permissions

- Android: the plugin's `istmo.toml` declares `android.permission.CAMERA`;
  `istmo-build` adds it to the app's `AndroidManifest.xml`.
- iOS: `NSCameraUsageDescription` in `Info.plist`. The plugin ships a
  default string; override it from your app's `[app.info_plist]` table.

Prompt the user from Rust:

```rust,ignore
match camera.request_access().await? {
    AccessStatus::Granted => { /* open sessions */ }
    AccessStatus::Denied | AccessStatus::Restricted => {
        // deep-link to Settings, surface rationale, etc.
    }
    AccessStatus::NotDetermined => { /* the prompt did not resolve */ }
}
```

## Minimal capture loop

```rust,ignore
use std::sync::Arc;
use istmo_camera::{
    AccessStatus, CameraClient, CameraClientExt, CameraFacing, CameraInitConfig,
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
                // frame.bytes holds the pixels; frame.planes describes
                // the layout. Hand to a decoder, GPU uploader, ML
                // inference pass, …
            }
            StreamItem::Completed | StreamItem::Cancelled => break,
            StreamItem::Failed(()) => return Err("camera stream failed".into()),
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

```rust,ignore
let back = cam.open_session(SessionConfig::new(back_id)).await?;
let front = cam.open_session(SessionConfig::new(front_id)).await?;

let (bf, ff) = (back.frames()?, front.frames()?);
// Interleave both streams in your own task.
```

Check `cam.concurrent_support().await?` ahead of time:

- `max_concurrent == 1` → fail over to switching between sessions
  instead of opening both.
- `approved_pairs` → only the listed `(primary, secondary)` tuples
  are safe on Android. Open any other pair at your own risk.
- `any_combination` → iOS multi-cam, pair any two visible cameras.

## Controls

```rust,ignore
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

On Android and iOS, `AppliedControls` reports the effective state —
the backend clamps values that fall outside what the hardware supports.
The web host reads the result back from `MediaStreamTrack.getSettings()`
(zoom, torch, exposure compensation, white-balance mode); the desktop
host reads zoom, exposure and manual focus back from the driver, in the
driver's integer steps. Fields neither host can read back echo the
request (fields left `None` report their defaults).

## Pixel formats and backpressure

Streaming raw frames to Rust is a memcpy per plane per frame. Pick a
pixel format matched to your consumer:

- `PixelFormat::Nv21` / `PixelFormat::Yuv420` — cheapest native path,
  ~1.5 bytes per pixel. Good for GPU uploads and ML models that
  accept YUV.
- `PixelFormat::Bgra8` / `PixelFormat::Rgba8` — 4 bytes per pixel.
  Convenient but ~2.5× the bandwidth of YUV.
- `PixelFormat::Jpeg(quality)` — stills only on Android and iOS
  (`capture_still`, hardware-encoded); opening a stream with it returns
  `CameraError::FormatUnsupported`.

Every backend keeps one ring buffer per `frames()` subscriber, sized
by `CameraInitConfig::frame_pool_depth` (default `3`). When a
subscriber falls behind, its oldest frame is dropped; other subscribers
are unaffected. Each delivered frame reports `dropped_before` so
Rust-side code can log, downsample, or back off.

For high-resolution preview-only pipelines, open the session with
`SessionConfig::preview_only()` — the stream stays silent and no
copy fires per frame. (The native widget rendering path is the
app's responsibility.)

## Still capture

`session.capture_still(StillCaptureRequest::default())` returns a
`StillPhoto`. Android (`ImageCapture`) and iOS (`AVCapturePhotoOutput`)
capture `PixelFormat::Jpeg` only and report
`CameraError::FormatUnsupported` for other formats; the desktop host
returns the latest preview frame in the session's stream format.

## Desktop (Linux, macOS, Windows)

Pure-Rust host behind the `desktop` feature:

```toml
[dependencies]
istmo-camera = { version = "0.1.1-alpha.5", features = ["desktop"] }
```

Backed by [`nokhwa`](https://docs.rs/nokhwa) — V4L2 on Linux,
AVFoundation on macOS, Media Foundation on Windows. Supports
`PixelFormat::Rgba8` and `PixelFormat::Bgra8` (nokhwa decodes the
camera's native YUYV / NV12 / RAWRGB stream into packed 32-bit).
Other formats return `CameraError::FormatUnsupported` on `open`.

Register the host with the runtime:

```rust,ignore
use istmo_camera::{CameraHost, DesktopCameraFactory};

runtime.register_host(CameraHost::new(DesktopCameraFactory));
```

NixOS builds pull the backend's C deps through `nix develop` —
`v4l-utils`, `libclang`, `gcc` and `nasm` are already in the
project's `flake.nix` devshell.

## Web (wasm32)

Browser host behind the `web` feature, built on
`navigator.mediaDevices.getUserMedia`: frames are drawn into a hidden
canvas and shipped as `PixelFormat::Rgba8` (or `Bgra8`); other formats
return `CameraError::FormatUnsupported`.

```toml
[target.'cfg(target_arch = "wasm32")'.dependencies]
istmo-camera = { version = "0.1.1-alpha.5", features = ["web"] }
```

## Native layer

Reference backends live under `native/{android,ios}/` and are
registered by the generated `IstmoPluginRegistry`:

- `CameraFactoryImpl.kt` / `CameraBackendImpl.kt` — CameraX pipeline
  with `ImageAnalysis` (`STRATEGY_KEEP_ONLY_LATEST`) fanned out to
  every frame subscriber; `Camera2CameraInfo` interop pulls full
  capability metadata. The factory takes the host
  `ComponentActivity` and follows activity recreation.
- `CameraFactoryImpl.swift` / `CameraBackendImpl.swift` —
  `AVCaptureSession` (or `AVCaptureMultiCamSession` when available)
  with `AVCaptureVideoDataOutput`.

Both implement `HandleReleaser` so `NativeHandle::drop` on the Rust
side tears the session down deterministically.

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

Full guide: <https://sergioribera.rs/istmo/plugins/camera/>
