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

Android, iOS, macOS, Windows and the web are designed
(`design/screen-capture.md`) but not implemented yet. Their hosts answer
`CaptureError::NoBackendCompiled`.

## Install

```toml
[dependencies]
istmo-screen-capture = "0.1.1-alpha.7"
```

The default features build every Linux backend except KMS. They need
`libpipewire` and `libgbm` at build time (pkg-config) and at run time.

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
        FormatTier::cpu_rgb(), // shared memory: allows CopyStage::ShmTransport
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

## Audio only

`open_audio_session` captures audio without a video session. The rate,
channels and format are asked per open; PipeWire converts what the
source does not have:

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
window and `RandR`.

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

Design: `design/screen-capture.md`. Platform facts:
`design/screen-capture-platforms.md`.
