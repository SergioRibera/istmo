# istmo-screen-capture

Screen capture plugin for the [istmo](https://github.com/SergioRibera/istmo)
framework: screenshots and live streams of monitors, windows and
regions.

- **Honest capability report.** Each backend lists its formats, memory,
  copies, cursor, damage and sync modes. Every section says whether it
  was probed, comes from a table, or is only known after consent.
- **Explicit selection.** Backends are chosen `Auto`, `Fixed` or
  `Ordered`; formats as ordered tiers. A requirement is met, or the
  call fails and names it. Downgrades go only to tiers you listed, and
  arrive as an event.
- **Zero copy where the platform allows it.** Frames go through native
  frame sinks as DMA-BUFs or mappings of shared memory. Every copy the
  plugin makes is named (`CopyStage`), in the report and on the frame.
- **Raw frames only.** Nothing is encoded: no PNG, no video.

| Backend | Feature | Memory | Consent | In `Auto` |
| --- | --- | --- | --- | --- |
| ext-image-copy (Sway 1.11, Hyprland, COSMIC) | `linux-ext-image-copy` | DMA-BUF, `wl_shm` | none | yes |
| wlr-screencopy (wlroots, niri) | `linux-wlr-screencopy` | DMA-BUF, `wl_shm` | none | yes |
| xdg-desktop-portal + PipeWire | `linux-portal` | DMA-BUF, `MemFd` | dialog per session (restore tokens) | no: by name only |
| X11 (XShm, Composite, XDamage, XFixes) | `linux-x11` | shared memory | none | yes |
| PipeWire audio (alone or with video) | `linux-audio` | interleaved PCM | none | — |
| Windows.Graphics.Capture (Windows 10 1903+) | `windows-wgc` | D3D11 texture, CPU | none (the OS draws a yellow border) | no: by name only |
| DXGI Desktop Duplication (monitors) | `windows-dxgi` | D3D11 texture (NT handle), CPU | none | no: by name only |
| GDI `BitBlt` / `PrintWindow` | `windows-gdi` | CPU | none | no: by name only |
| WASAPI loopback audio (alone or with video) | `windows-audio` | interleaved PCM | none | — |
| macOS ScreenCaptureKit 12.3+ (displays, windows, apps; audio 13+) | `macos-sck` | `IOSurface`, `CVPixelBuffer`, locked `IOSurface` memory | Screen Recording (TCC); picker on 14+ | no: by name only |
| iOS / iPadOS 27 ScreenCaptureKit (Swift) | — | `CVPixelBuffer`, `IOSurface` | system picker per session | no: by name only |
| iOS ReplayKit in-app, before 27 (Swift) | — | `CVPixelBuffer`, `IOSurface` | system alert per session | no: by name only |
| Android MediaProjection (Kotlin, `native/android`) | — | `HardwareBuffer` (API 29+), CPU | system dialog per session | yes (the only one) |

The Windows backends stay out of `Auto` until their checklist passed on
a real Windows session (`examples/win_probe.rs` runs it); name them with
`BackendSelection::Fixed` / `Ordered`. The Apple backends compile and
pass their lints but have not run on a Mac, iPhone or iPad yet: they
stay out of `Auto` until the checklist in `docs/apple-checklist.es.md`
passes. The web is designed (`design/screen-capture.md`) but not
implemented yet; its host answers
`CaptureError::NoBackendCompiled`.

## Install

```toml
[dependencies]
istmo-screen-capture = "0.1.1-alpha.7"
```

The default features build every Linux backend except KMS, every
Windows backend and ScreenCaptureKit on macOS; each platform's
dependencies are gated on its target. On Linux they need `libpipewire`
and `libgbm` at build time (pkg-config) and at run time. On Windows
nothing beyond the OS.


Register the desktop host with the runtime:

```rust,ignore
use istmo_screen_capture::{CapturePolicy, DesktopCapture, ScreenCaptureHost};

runtime.register_host(ScreenCaptureHost::new(DesktopCapture::with_policy(
    CapturePolicy::default(),
)));
```

`CapturePolicy` is the app's own rule set; nothing on the bus can change
it:

- Remote processes are refused unless the policy opens a method. Even
  then, sessions answer only the origin that opened them, and restore
  tokens never leave the process.
- `allowed_backends` limits which backends may capture.
- KMS needs both `allow_kms` and the `linux-kms` feature.

## A screenshot as pixels

`screenshot_cpu` returns raw, upright pixels in the byte order you ask
for. It always reads through CPU memory and never maps a GPU buffer.

```rust,ignore
use std::sync::Arc;
use istmo_screen_capture::{
    CpuPixelFormat, PixelLayout, ScreenCaptureClient, ScreenCaptureClientExt,
    ScreenshotRequest, SourceSelector,
};

async fn shot(client: &Arc<ScreenCaptureClient>) -> Result<(), Box<dyn std::error::Error>> {
    let image = client
        .screenshot_cpu(
            ScreenshotRequest::of(SourceSelector::AllMonitors),
            PixelLayout { format: CpuPixelFormat::Rgba8, upright: true, tight: true },
        )
        .await?;
    // image.bytes: image.height rows of image.stride bytes, RGBA.
    println!("{}x{}", image.width, image.height);
    Ok(())
}
```

Several monitors are composited into one image at the highest monitor
scale (`CopyStage::Composite`). `ScreenshotRequest::composite(false)`
returns one frame per monitor instead, through `screenshot_frames`.

## A live session

```rust,ignore
use std::sync::Arc;
use istmo_core::{Runtime, SinkPolicy};
use istmo_screen_capture::{
    BufferMemory, Exhaustion, FrameRate, ScreenCaptureClient, ScreenCaptureClientExt,
    SessionConfig,
};

async fn stream(
    runtime: &Arc<Runtime>,
    client: &Arc<ScreenCaptureClient>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = SessionConfig::primary_monitor()
        .fps(FrameRate::Max(60))
        .exhaustion(Exhaustion::Stall);
    let session = client.open_session(runtime, config, SinkPolicy::queue(3)).await?;
    let negotiated = session.negotiated();
    println!("{} {:?} via {:?}, copies {:?}",
        negotiated.backend, negotiated.format, negotiated.memory, negotiated.copies);

    while let Some(frame) = session.video().recv() {
        match frame.meta.memory {
            // Import frame.buffer.dmabuf() on the GPU (Vulkan, VA-API,
            // NVENC); cache imports by frame.meta.buffer (BufferIdentity).
            BufferMemory::DmaBuf => {}
            // frame.buffer.as_bytes(): frame.meta.planes[0].stride per row.
            _ => {}
        }
        // Dropping the frame hands its buffer back to the producer.
    }
    session.close().await?;
    Ok(())
}
```

**Default formats.** An empty format request, which is the default,
takes the backend's native memory. That is often a DMA-BUF, and only a
GPU consumer can read one. A consumer that reads `as_bytes()` asks for
CPU frames:

```rust,ignore
let config = SessionConfig::primary_monitor().cpu_frames();
```

Or list tiers best-first. Each tier names the copies it accepts:

```rust,ignore
use istmo_screen_capture::{FormatChoice, FormatRequest, FormatTier, PixelFormat};

let formats = FormatRequest {
    tiers: vec![
        FormatTier::new(vec![FormatChoice::dmabuf(PixelFormat::Bgrx8, Vec::new())]),
        FormatTier::cpu_rgb(), // shared memory: allows CopyStage::ShmTransport / GpuReadback / DxgiMandatory
    ],
};
let config = SessionConfig::primary_monitor().formats(formats);
```

**Pools.** A consumer never receives a producer's last free buffer.
While it holds `pool_depth - 1` frames, `Exhaustion` decides what
happens:

- `Stall` pauses capture until a frame drops.
- `DropNewest` discards new frames.
- `CopyToCpu` keeps frames flowing as CPU read-backs, marked
  `ExhaustionReadback`.

Each episode emits `CaptureEvent::Exhausted` once.

**Events.** `session.events()` replays the negotiated stream to every
subscriber, then streams these events:

- `Renegotiated` (for example when the first DMA-BUF names its modifier)
- `Downgraded`
- `SourceResized`
- `Paused` / `Resumed`
- `Exhausted`
- `Stopped`, which says why the session ended

## Windows

- **WGC** captures monitors and windows. With an empty format request
  (or a `D3d11Texture` tier) frames are the frame pool's own textures,
  no copy: `frame.buffer.downcast_ref::<D3d11Frame>()` gives the
  `ID3D11Texture2D` and the plugin's device (multithread-protected, so
  it may be used from the consumer's threads). Those textures cannot be
  shared with another device; for that, pass
  `GpuPreference::Same(GpuIdentity::Luid(..))` and a tier allowing
  `CopyStage::CrossDevice`: each frame is then copied into a texture
  with an NT handle (`D3d11Frame::shared_handle`, for
  `ID3D11Device1::OpenSharedResource1`). CPU frames are read back
  through a staging texture (`CopyStage::GpuReadback`). FP16 (scRGB)
  with `HdrPolicy::PreferHdr` / `RequireHdr`.
- **DXGI** captures monitors; every frame is copied into the plugin's
  pool (`CopyStage::DxgiMandatory`), whose textures carry NT handles.
  Rotated outputs arrive unrotated (`FrameMeta::orientation` says how
  to turn them), the pointer only as `CursorMode::Metadata`, damage
  from dirty and move rectangles, `protected_content` when DXGI masked
  something. The secure desktop pauses the session
  (`Paused(SecureDesktop)`).
- **GDI** is the CPU fallback (`BitBlt`, `PrintWindow` for windows).
  `PrintWindow` waits for the window's thread to handle `WM_PRINT`, so
  GDI refuses the app's own windows (that thread may be the one awaiting
  the capture; use WGC for them), skips windows that stopped answering,
  and gives other windows 500 ms per frame: a screenshot then fails, a
  stream drops the frame and fails after 20 misses in a row.
- No fence or keyed mutex travels with a frame: the plugin pushes it
  once the GPU finished writing (`SyncKind::Ready`), and dropping it
  means every read of it is done.
- Geometry is in physical desktop pixels, so `scale_permille` is 1000.
- `pts_ns` is on `PtsClock::Qpc`; `wall_us` is UNIX-epoch µs as
  everywhere.
- Borderless WGC capture: `WindowsOptions::borderless` (`Prefer` /
  `Require`) with `CapturePolicy::allow_borderless`, build 20348+. The
  OS consent runs at most once; `access(AccessScope::BorderlessCapture)`
  reads it without prompting, `request_access` prompts (on a plugin
  thread, up to 2 min). Opening a session never prompts: `Require`
  before an answer is `ConsentRequired`. The capability lives in a
  package manifest (`graphicsCaptureWithoutBorder`), so unpackaged apps
  read `Restricted`. A hidden border is the best case: another app
  capturing the same window with a border shows it anyway.
- Audio: `AudioSource::System` (default render endpoint loopback),
  `ExcludeSelf` and `Application(executable or pid)` (process loopback,
  build 20348+).

## Audio only

`open_audio_session` captures audio without a video session. The rate,
channels and format are asked per open; PipeWire or the Windows audio
engine converts what the source does not have:

```rust,ignore
use istmo_core::{Overflow, SinkPolicy};
use istmo_screen_capture::{AudioConfig, AudioSource, AudioUpdate, SampleFormat};

let config = AudioConfig::of(AudioSource::System)
    .rate(44_100)
    .channels(2)
    .format(SampleFormat::S16);
let session = client
    .open_audio_session(&runtime, config, SinkPolicy::queue(32).with_overflow(Overflow::DropNewest))
    .await?;
while let Some(chunk) = session.audio().recv() {
    // chunk.meta.wall_us: UNIX-epoch µs, following the sample count;
    // chunk.buffer.as_bytes(): interleaved PCM, chunk.meta.frames frames.
}
session.update(AudioUpdate::Mute).await?; // on time, silent
```

The handle has its own `update` (`Pause`, `Resume`, `Mute`,
`Unmute`), `close` and `events` (`AudioEvent`). Dropping it closes the
session. `capabilities(..).audio` says whether audio capture runs
here.

## Sources

`sources` lists monitors and windows, for local callers only by default
(titles and app ids are sensitive). Monitors carry `make`, `model`,
`connector` and `description` where the platform reports them.

On Wayland the windows come from the compositor's toplevel list
(`ext_foreign_toplevel_list_v1`, else
`zwlr_foreign_toplevel_manager_v1`), also when the backend that lists
them cannot capture windows (wlr-screencopy, niri). `Source::capture`
says which is which:

- `ById`: `SourceSelector::Id` captures it.
- `PickerOnly(backends)`: listed only. An `Id` is
  `Unsatisfiable(WindowById)`; a picker of one of `backends` (the
  portal, with `allow_picker`) can capture it.

`watch_sources` streams the same listing as it changes: `Listed` first,
then `Added`, `Changed` (title, geometry, mode, focus) and `Removed`.
It is local-only whatever the policy. `source_watch` keeps the current
list for you:

```rust,ignore
let mut watch = client.source_watch(SourceFilter::default()).await?;
println!("{} sources", watch.sources().len());
while let Some(change) = watch.recv() {
    println!("{:?}; now {} sources", change?, watch.sources().len());
}
```

Wayland watches follow output hot-plug (`wl_output` / xdg-output) and
the toplevel list; X11 watches follow the root window, every listed
window and `RandR`. Windows watches follow `WM_DISPLAYCHANGE` and
window events (`SetWinEventHook`: shown, hidden, destroyed, renamed,
moved, minimised, cloaked, focused). Windows lists only what a person
sees as an app window, with its executable name as `app_id`.

## Consent

Direct Wayland and X11 capture need no consent. The portal shows its
dialog inside `open` / `screenshot`, and only when
`ConsentConfig::allow_picker` is set; otherwise the call fails with
`ConsentRequired`.

With `persist` set, a restore token comes back in `NegotiatedStream`.
Store it in the platform secret store and pass it back to skip the
dialog. Each token works once; replace the stored one after every
session.

The plugin never gates capture on consent. Each backend's
`ConsentModel` is in the report, so an app that wants consent-backed
capture can exclude `NoneNeeded` backends itself.

## macOS

`macos-sck` (on by default, inert elsewhere) captures displays, windows
and applications through ScreenCaptureKit. It needs the Screen Recording
grant:

- `access(AccessScope::ScreenRecording)` never prompts. `Granted`,
  `NotDetermined`, or `RequiresRestart` once this process asked: macOS
  applies a new grant to a relaunched process only.
- `request_access` shows the system prompt (at most once per process).
- Without the grant nothing is listed (`SourceListing::AfterGrant`) and
  capture answers `ConsentRequired`; the plugin never lets the framework
  prompt by itself.

Frames are the stream's own `IOSurface`s, retained until the frame
drops. Ask for `BufferMemory::IoSurface` or `CvPixelBuffer` and read the
backing through `frame.buffer.downcast_ref::<IoSurfaceFrame>()`
(`io_surface_ptr`, `pixel_buffer_ptr`, `surface_id`), or ask for `Cpu`:
the surface locked read-only, still no copy. Formats: `Bgra8`, `Ar30`,
`Nv12` (video range). Timestamps are host time
(`PtsClock::HostTime`); `wall_us` is the UNIX-epoch time of each frame.

`SourceSelector::Picker` shows the system's content-sharing picker on
14+ (with `allow_picker`; the app needs a running `NSApplication`).
System or application audio needs macOS 13.

## iOS and iPadOS

The Swift backend in `native/ios` is picker-only: set
`ConsentConfig.allowPicker`, and name a backend (`.fixed(.iosSck)` on
iOS 27, `.fixed(.iosReplayKit)` before) until they join `Auto`.
ScreenCaptureKit (27) captures what the person picks; a picker offering
only `SourceKind::Application` presents it for this app's own content
(`presentForCurrentApplication`). A full-display capture keeps running
in the background only when the app declares the `screen-capture`
background mode (`UIBackgroundModes`; App Store review of that key is
unconfirmed), otherwise the session reports `Paused(Backgrounded)`
while backgrounded. ReplayKit captures the app's own screen in the
foreground only. Frames reach Rust as
retained `CVPixelBuffer`s / `IOSurface`s. `open_preview` renders into an
`AVSampleBufferDisplayLayer` registered with
`IstmoRuntime.shared.registerObject(layer)`.

## Android

The Kotlin backend in `native/android` captures through MediaProjection.
It needs the host `ComponentActivity` (the consent dialog goes through
its `ActivityResultRegistry`) and runs every projection under
`IstmoProjectionService`, a non-exported foreground service of type
`mediaProjection` that the plugin's manifest declares with its
permissions. Apps that capture audio add `RECORD_AUDIO` themselves.

- Consent: Android asks every time. `open` / `screenshot` show the
  dialog when `allow_picker` is set; `request_projection` shows it ahead
  of time and returns a `ProjectionGrant` for one later call (gone after
  60 s or when the projection stops). While it waits the projection
  already runs, notification and status-bar chip included: ask for it
  right before using it. Audio-only sessions take such a grant:
  `AudioConfig` has no consent fields. From API 34 a picker source lets
  the person share one app instead of the screen.
- One projection at a time: Android stops a running projection when a
  new one starts, so a call that needs a new consent while a session,
  a screenshot or a grant is live answers `BackendUnavailable`
  (`NotOnThisSession`) instead. Pass the grant, or close the session
  first.
- Frames: a virtual display mirrors the default display into an
  `ImageReader`. The native tier is `RGBA_8888` `HardwareBuffer`s handed
  over without a copy; each `Image` stays open until Rust drops the
  frame, and the consumer never gets the reader's last image. A CPU tier
  copies each image into the sink (`CopyStage::GpuReadback`). Frames
  come already upright; rotation and app-window resizes give a new pool
  (`pool_generation`) and `SourceResized`.
- Preview: `open_preview` renders straight into a `Surface` the app
  registered (`IstmoRuntime.registerObject(holder.surface)`), with no
  copy. `PreviewTarget::fit` applies only to
  `open_preview_with_frames`, where a GLES pass draws each frame into
  the preview and the sink (`CopyStage::PreviewFanOut`).
- Audio: playback capture (API 29+) of media, game and unknown usages,
  one app's (`AudioSource::Application(package)`) or everything but this
  app's. Apps that opt out, or target API 28 and lower, are silent.
  In the background (API 30+) capture may go silent: the service is
  typed `mediaProjection` only (not yet verified on a device).
- Stopping from the notification, the status-bar chip or the lock
  screen ends sessions with `ConsentRevoked`.

## Kotlin and Swift

The generated clients offer the control plane:

- `backends`
- `capabilities`
- `sources`, `watch_sources`
- `screenshot_pixels`
- `open_preview`
- updates and events

Methods that take a frame sink are Rust-only. `ScreenCaptureDefaults`
in `native/android` and `native/ios` builds configs with the same
defaults as Rust's `Default`.

Swift names the message types `ScreenCapture.<Type>`
(`ScreenCapture.SessionConfig`) and the shared permission state
`IstmoMedia.AccessStatus`; Kotlin imports it from `dev.istmo.media`.

## Testing

Unit tests run on fake backends. The real backends have gated live
tests:

- `scripts/test-screen-capture-sway.sh`: headless sway
- `scripts/test-screen-capture-x11.sh`: Xvfb
- `ISTMO_SC_TEST_NIRI=1`: a live niri session
- `ISTMO_SC_TEST_NIRI_SCREENCAST=1`: niri's PipeWire producer, no dialog
- `ISTMO_SC_TEST_PORTAL_SESSION=1`: one portal dialog
- `ISTMO_SC_TEST_PIPEWIRE=1`: audio-only sessions at 44.1 and 48 kHz, no dialog
- `examples/win_probe.rs`: the whole checklist on a Windows desktop
  (build it with `--target x86_64-pc-windows-msvc`); prints PASS / FAIL
  and writes `win_probe_report.txt`
- `ISTMO_SC_TEST_MACOS=1` (plus `_PICKER`, `_AUDIO`): ScreenCaptureKit on a
  Mac (`tests/macos_live.rs`); the full Apple checklist, iOS included,
  is `docs/apple-checklist.es.md`
- `examples/android-probe`: an Android app that runs each Android check
  on a device (see its `README.md`)

Design: `design/screen-capture.md`. Platform facts:
`design/screen-capture-platforms.md`.
