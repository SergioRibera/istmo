# `istmo-screen-capture`

Design for a cross-platform screen capture plugin: screenshots and
live streams of monitors, windows, applications and regions, with a
complete capability report, explicit backend selection, consent
modelled as state, a control plane over istmo and a data plane over
native frame sinks (`design/native-frames.md`). Not implemented yet;
this document is for review.

## Goals

- One API for Linux (Wayland + X11), Windows, macOS, Android, iOS and
  the web, callable from Rust, Kotlin and Swift.
- Zero-copy where the platform allows it: frames stay in the memory the
  compositor / OS produced (DMA-BUF, D3D11 texture, `IOSurface`,
  `AHardwareBuffer`) until the consumer imports them.
- Nothing silent: a backend, format, memory type or feature the caller
  asked for is delivered or the call fails with an error that says what
  was missing and where. No quiet fallback to a CPU copy, a different
  backend or a lower frame rate.
- Report everything a consumer needs to plan: formats, modifiers,
  limits, colour, cursor, damage, orientation, the producing GPU,
  audio, consent and persistence — per source and per backend.
- Frame types owned by this plugin. ferricast and sss become
  consumers; neither is a dependency.

Non-goals: encoding, recording to files, remote desktop input
injection, drawing overlays. Those belong to consumers.

## Crate layout

```
plugins/screen-capture/                 crate `istmo-screen-capture`, id `istmo.screen_capture`
  istmo.toml                            plugin manifest + static permissions
  src/lib.rs                            trait, wire types, client sugar (SessionHandle)
  src/types/{source,caps,config,meta,consent,error}.rs
  src/select.rs                         Auto / Fixed / Ordered resolution, shared by every host
  src/desktop.rs                        Rust host: backend registry + session table
  src/desktop/linux/{portal,pipewire,ext_image_copy,wlr_screencopy,x11,kms,dmabuf_alloc}.rs
  src/desktop/windows/{wgc,dxgi,gdi,d3d11}.rs
  src/desktop/macos/{sck,cg_window,iosurface}.rs
  src/web.rs                            getDisplayMedia host (wasm32)
  native/android/                       ScreenCaptureBackendImpl.kt + IstmoProjectionService.kt
  native/ios/                           ScreenCaptureBackendImpl.swift (ReplayKit in-app)
  native/ios-broadcast/                 Broadcast Upload Extension template (SampleHandler.swift)
  tests/
```

Backends are cargo features (`linux-portal`, `linux-ext-image-copy`,
`linux-wlr-screencopy`, `linux-x11`, `linux-kms`, `windows-wgc`,
`windows-dxgi`, `windows-gdi`, `macos-sck`, `macos-cg`), all default
except `linux-kms` (needs privileges). Platform dependencies are
target-gated so features are inert off their OS; a build that compiles
no backend for its target still builds and answers
`NoBackendCompiled` at run time (ferricast's lesson: `compile_error!`
messages drift).

## Control plane

Stateless plugin (sessions are native handles), so the typed
in-process shortcut applies to Rust consumers of the desktop host.

```rust
#[istmo::plugin(name = "istmo.screen_capture")]
pub trait ScreenCapture {
    /// Compiled backends, whether each can run here, and why not.
    async fn backends(&self) -> Vec<BackendStatus>;

    /// Capabilities of every runnable backend (or one) for a source
    /// kind or a concrete source.
    async fn capabilities(&self, query: CapabilityQuery) -> Result<CapabilityReport, CaptureError>;

    /// Monitors, windows and applications the process may capture
    /// without a picker. Empty with `SourceListing::PickerOnly` where
    /// the platform forbids enumeration (portal, Android, iOS, web).
    async fn sources(&self, filter: SourceFilter) -> Result<SourceList, CaptureError>;

    /// Consent / permission state, without prompting.
    async fn access(&self, scope: AccessScope) -> Result<AccessState, CaptureError>;

    /// Prompt (TCC request, MediaProjection intent, portal dialog).
    async fn request_access(&self, request: AccessRequest) -> Result<AccessState, CaptureError>;

    /// One frame into `sink`, then done. Same selection rules as `open`.
    async fn screenshot(&self, request: ScreenshotRequest, sink: FrameSinkId<FrameMeta>)
        -> Result<ScreenshotInfo, CaptureError>;

    /// Screenshot as encoded PNG / raw bytes over the wire: the path for
    /// consumers that cannot reach a sink (other process, Kotlin/Swift).
    async fn screenshot_encoded(&self, request: ScreenshotRequest, encoding: ImageEncoding)
        -> Result<EncodedImage, CaptureError>;

    /// Start a live session; frames go to `video`, audio to `audio`.
    #[owned]
    async fn open(&self, config: SessionConfig, video: FrameSinkId<FrameMeta>,
                  audio: Option<FrameSinkId<AudioMeta>>) -> Result<CaptureSession, CaptureError>;

    /// Change frame rate, cursor mode, region, pause / resume. Returns
    /// the renegotiated stream, or an error leaving the session as it was.
    async fn update(&self, session: NativeHandleId, update: SessionUpdate)
        -> Result<NegotiatedStream, CaptureError>;

    async fn close(&self, session: NativeHandleId) -> Result<(), CaptureError>;

    /// Lifecycle events: renegotiated, source resized / lost / hidden,
    /// paused by the system, consent revoked, restore token issued,
    /// backend error.
    #[stream]
    fn events(&self, session: NativeHandleId) -> Result<CaptureEvent, CaptureError>;
}
```

`CaptureSession { session: NativeHandleId, negotiated: NegotiatedStream }`
with `#[handle(CaptureSessionTag)]`, so `open_owned` returns a session
that closes on drop. A Rust `SessionHandle` sugar mirrors camera's.

## Sources

```rust
pub struct Source {
    pub id: SourceId,              // opaque, backend-scoped: "wl-output:DP-1", "hwnd:0x1234", "sck-display:1"
    pub kind: SourceKind,          // Monitor | Window | Application | Region | Picker
    pub name: String,              // output name / window title / app name
    pub app_id: Option<String>,    // Wayland app_id, bundle id, AUMID, package
    pub pid: Option<u32>,
    pub geometry: Option<Rect>,    // logical, in the desktop's global space
    pub physical_size: Option<Size>,
    pub scale: Option<f32>,
    pub refresh_mhz: Option<u32>,
    pub transform: Orientation,
    pub flags: SourceFlags,        // primary, minimised, occluded, protected (DRM/secure), self
}
pub enum SourceSelector {
    Id(SourceId),
    Region { monitor: Option<SourceId>, rect: Rect },   // monitor-relative or global
    Picker(PickerOptions),        // portal dialog, SCContentSharingPicker, MediaProjection, getDisplayMedia
    PrimaryMonitor, AllMonitors, Focused, UnderCursor,
}
```

Regions are crops, not sources, on every platform; where the backend
crops natively (portal `SPA_META_VideoCrop`, SCK `sourceRect`, WGC
none, DXGI none) the capability report says `RegionCrop::Native`,
otherwise `RegionCrop::Consumer` and the crop travels in `FrameMeta`
(`content_rect`) — the plugin never copies to crop.

## Capability report

```rust
pub struct CapabilityReport { pub backends: Vec<BackendCaps> }
pub struct BackendCaps {
    pub backend: BackendId,
    pub source_kinds: Vec<SourceKind>,
    pub listing: SourceListing,                 // Enumerable | PickerOnly
    pub video: Vec<FormatCaps>,                  // see below
    pub max_fps: Option<u32>, pub min_interval: Option<Duration>,
    pub max_size: Option<Size>, pub scaling: Scaling,     // Native | Backend | None
    pub region: RegionCrop,
    pub cursor: Vec<CursorMode>,                 // Hidden | Embedded | Metadata
    pub damage: DamageSupport,                   // None | Frame | Rects
    pub orientation: OrientationSupport,         // Reported | AlreadyApplied | None
    pub colour: ColourCaps,                      // spaces, transfer (SDR / PQ / HLG / scRGB), bit depths, HDR metadata
    pub audio: AudioCaps,                        // None | System | PerApplication | ExcludeSelf + rates/channels
    pub producer: Option<GpuIdentity>,           // DRM dev_t / DXGI LUID / Metal registryID / none
    pub sync: Vec<SyncKind>,                     // Implicit | ExplicitTimeline | ReadyOnDelivery
    pub consent: ConsentModel,                   // see consent
    pub persistence: Persistence,                // None | RestoreToken | SystemSetting
    pub concurrency: Option<u32>,                // sessions per process
    pub window_capture: WindowCaptureCaps,       // occluded content, child popups, border
    pub notes: Vec<CapabilityNote>,              // typed caveats: "nvidia-implicit-sync-best-effort", ...
}
pub struct FormatCaps {
    pub format: PixelFormat,                     // BGRA8, RGBA8, BGRX8, XR30, RGBA16F, NV12, P010, ...
    pub memory: MemoryKind,                      // Cpu | DmaBuf | D3d11Texture | IoSurface | HardwareBuffer | CvPixelBuffer | WebVideoFrame
    pub modifiers: Vec<u64>,                     // DMA-BUF: the set the backend can produce, LINEAR included when allowed
}
```

`backends()` returns each compiled backend with `Availability`:
`Ready`, `NotOnThisSession { reason }` ("no wlr-screencopy global",
"running on X11"), `MissingPermission`, `OsTooOld { required }`,
`NotCompiled { feature }` (for the report only; `Fixed` naming it fails
with the same data).

Every report entry is derived from probing, not from tables: protocol
globals, PipeWire `EnumFormat` params, `linux-dmabuf` feedback tranches,
`IDXGIOutput6::GetDesc1`, `SCShareableContent`, `MediaProjection`
API level. Probes are cached per session start and invalidated on
output hot-plug.

## Configuration and selection

```rust
pub struct SessionConfig {
    pub source: SourceSelector,
    pub backend: BackendSelection,
    pub video: VideoConfig,
    pub audio: Option<AudioConfig>,
    pub consent: ConsentConfig,
}
pub enum BackendSelection {
    Auto(AutoSelect),              // default
    Fixed(BackendId),
    Ordered(Vec<BackendId>),
}
pub struct AutoSelect { pub prefer: AutoPreference /* ZeroCopy | LowLatency | Compatibility */, pub exclude: Vec<BackendId> }
pub struct VideoConfig {
    pub formats: Vec<FormatRequest>,   // ordered; empty = backend's native
    pub fps: FrameRate,                // Max(u32) | Exact(u32) | OnDamage { max: u32 }
    pub size: OutputSize,              // Native | Scaled(Size) | Fit(Size)
    pub cursor: CursorMode,
    pub damage: bool,
    pub hdr: HdrPolicy,                // Sdr | PreferHdr | RequireHdr
    pub pool_depth: u8,                // producer buffers in flight
    pub gpu: GpuPreference,            // Auto | Same(GpuIdentity) | Discrete | Integrated
}
pub struct FormatRequest { pub format: PixelFormat, pub memory: MemoryKind, pub modifiers: Vec<u64> /* empty = any */ }
```

Resolution rules (shared `select.rs`, mirrors ferricast's codec
selection):

- `Auto` walks runnable backends in the platform order below, filtered
  by `exclude`, and picks the first whose probe satisfies **every**
  requirement in the config. Requirements are what the caller wrote:
  a `FormatRequest` list means "one of these", `RequireHdr` means HDR,
  `damage: true` means damage rects. Preferences only reorder.
- `Fixed(id)` uses that backend or fails with its error; `Ordered`
  tries the list in order, then fails with every reason collected.
- Environment override `ISTMO_SCREEN_CAPTURE_BACKEND` (`auto`, a
  backend name or a comma list) replaces `Auto` only.
- Errors: `BackendNotCompiled { backend, feature }`,
  `BackendUnavailable { backend, reason }`,
  `Unsatisfiable { backend, unmet: Vec<Requirement> }`,
  `NoBackend { tried: Vec<(BackendId, CaptureError)> }`,
  `NoBackendCompiled`.
- Mid-session failure never switches backend: the session reports
  `CaptureEvent::Failed` and ends; the consumer reopens.

Auto order per platform (zero-copy first):

| Platform | Order |
|---|---|
| Linux Wayland | ext-image-copy-capture → wlr-screencopy → portal (PipeWire) |
| Linux, sandboxed (Flatpak / Snap) | portal only |
| Linux X11 | X11 (XShm / Composite) → portal |
| Windows | WGC (10 1903+) → DXGI duplication → GDI (screenshots only) |
| macOS | ScreenCaptureKit (12.3+) → CGWindowList (screenshots, < 15) |
| Android | MediaProjection |
| iOS | ReplayKit in-app; broadcast extension when `scope = System` |
| Web | getDisplayMedia |

Direct Wayland protocols come before the portal because they need no
dialog and give damage + explicit buffer control; the portal wins
automatically when they are absent (GNOME, KDE) or sandboxed. Window
capture on GNOME/KDE is portal-only.

## Data plane

Frames go to the consumer's `FrameSink<FrameMeta>`; nothing is encoded.

```rust
pub struct FrameMeta {
    pub seq: u64,
    pub pts_ns: u64,                 // capture time, monotonic clock of the platform (CLOCK_MONOTONIC, QPC, mach_continuous, elapsedRealtimeNanos)
    pub wall_us: u64,                // same instant, UNIX epoch µs (ferricast's segmenter needs wall time)
    pub size: Size,                  // buffer size
    pub content_rect: Rect,          // valid / cropped area inside the buffer
    pub format: PixelFormat,
    pub planes: Vec<PlaneLayout>,    // offset, stride (CPU and DMA-BUF)
    pub modifier: Option<u64>,
    pub colour: Colour,              // primaries, transfer, matrix, range, bit depth
    pub hdr: Option<HdrMetadata>,    // mastering display + CLL when the source has them
    pub orientation: Orientation,    // rotation + flip still to apply (Wayland output transform, Android display rotation)
    pub damage: Damage,              // Full | Rects(Vec<Rect>) | Unchanged (cursor-only frame)
    pub cursor: Option<CursorState>, // position, hotspot, visible; shape bytes only when `shape_serial` changes
    pub sync: FrameSync,             // Ready | Implicit | Timeline { point } (acquire fence in the buffer)
    pub scale: f32,
    pub source_id: SourceId,
}
```

Buffers per backend: Linux `DmaBuf` (or `Cpu` for shm), Windows a
plugin-owned `D3d11Texture` backing (`ID3D11Texture2D` + device + an
optional NT shared handle for another device), macOS `IoSurface`
backing (retained `IOSurfaceRef`, `CVPixelBuffer` kept alongside),
Android `istmo_android::HardwareBuffer`, iOS `istmo_ios::RetainedBuffer`
(`CVPixelBuffer`), web a `WebVideoFrame` backing (the JS `VideoFrame`,
closed on drop). Pooled producers attach `with_release` so a buffer
returns to the pool only when the consumer is done with it — the
pool depth (`pool_depth`) and the sink policy together are the
backpressure story; a consumer that holds every buffer stalls capture
instead of being overwritten.

Audio: `FrameSink<AudioMeta>` with `Cpu` buffers (interleaved PCM;
`AudioMeta { pts_ns, wall_us, rate, channels, format, frames }`).
Sources: PipeWire monitor / app nodes, WASAPI loopback (process
loopback on Windows 10 2004+), SCK `capturesAudio`,
`AudioPlaybackCaptureConfiguration`, ReplayKit `audioApp`,
getDisplayMedia audio.

## Consent and permissions

Consent is state, not an exception path:

```rust
pub struct AccessState {
    pub status: AccessStatus,        // Granted | Denied | NotDetermined | Restricted | PerSession
    pub model: ConsentModel,         // NoneNeeded | SystemSetting (TCC) | PerSessionDialog | PickerPerSession
    pub persistence: Persistence,    // None | RestoreToken | SystemSetting
    pub revocable_mid_session: bool,
}
pub struct ConsentConfig {
    pub restore_token: Option<RestoreToken>,   // portal / MediaProjection never; opaque, plugin-versioned
    pub persist: PersistMode,                  // DoNot | WhileRunning | UntilRevoked
    pub allow_picker: bool,                    // false → fail with ConsentRequired instead of showing UI
}
```

| Platform | Model | Persistence | Revocation |
|---|---|---|---|
| Wayland portal | dialog per session | portal restore token (`persist_mode` 2), returned in `NegotiatedStream` and `CaptureEvent::RestoreToken` | user closes stream → `ConsentRevoked` |
| Wayland direct / X11 | none | — | — |
| Windows | none (WGC border unless `graphicsCaptureWithoutBorder`, `GraphicsCaptureAccess` on 11) | — | — |
| macOS | TCC Screen Recording (`CGPreflightScreenCaptureAccess` / `CGRequestScreenCaptureAccess`); 15+ periodic re-prompt; `SCContentSharingPicker` avoids the global grant | system setting | `SCStreamDelegate didStopWithError` |
| Android | per-session `createScreenCaptureIntent`; API 34+ token single-use, must start the `mediaProjection` foreground service before `getMediaProjection`; app-only sharing on 14 QPR2+ | none | `MediaProjection.Callback.onStop` |
| iOS | in-app: system alert each `startCapture`; system-wide: `RPSystemBroadcastPickerView` | none | stop from Control Centre |
| Web | `getDisplayMedia` user gesture per call | none | track `ended` |

Static permissions come from the plugin's `istmo.toml` and land in the
app through `istmo-build` / nix-bundle-app:

- Android: `FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_MEDIA_PROJECTION`,
  `POST_NOTIFICATIONS` (service notification), `RECORD_AUDIO` only with
  the `audio` feature; the `<service android:foregroundServiceType="mediaProjection">`
  declaration (`IstmoProjectionService`).
- iOS: `NSMicrophoneUsageDescription` when mic audio is requested; the
  broadcast extension needs an App Group entitlement on both targets and
  an extension target (generated `project.yml` fragment).
- macOS: `NSScreenCaptureUsageDescription` (shown by the picker),
  `com.apple.security.device.audio-input` for mic, hardened-runtime
  friendly; no sandbox entitlement exists for screen recording (TCC).
- Windows (packaged): `graphicsCaptureProgrammatic` /
  `graphicsCaptureWithoutBorder` restricted capabilities via
  `[[windows_manifest_fragment]]`.
- Linux Flatpak: portal access is default; direct protocols need
  `--socket=wayland` (already) and are unavailable inside the sandbox
  in practice (the probe reports it).

## Backend notes (what each must get right)

**Portal + PipeWire** (port from ferricast `capture/src/pipewire`):
`CreateSession` / `SelectSources` (types, cursor_mode, persist_mode,
restore_token) / `Start`, `OpenPipeWireRemote` fd; stream negotiation
with `SPA_FORMAT_VIDEO_modifier` (`DONT_FIXATE`, the consumer's
modifiers first), `SPA_DATA_DmaBuf` before `MemFd`; `SPA_META_SyncTimeline`
explicit sync (wait acquire point, signal release point when the
consumer drops the buffer — the release hook), `SPA_META_VideoDamage`,
`SPA_META_Cursor`, `SPA_META_VideoTransform`, `SPA_META_VideoCrop`.
Buffers are held (not re-queued) until released, so the pool depth is
PipeWire's buffer count. Audio via the monitor node.

**ext-image-copy-capture-v1** (from sss `ext_image_copy`):
`ext_output_image_capture_source_manager_v1` /
`ext_foreign_toplevel_image_capture_source_manager_v1`; session
constraints (`buffer_size`, `shm_format`, `dmabuf_device`,
`dmabuf_format` + modifiers) drive allocation; damage per frame;
cursor sessions for `CursorMode::Metadata`; `presentation_time`.

**wlr-screencopy** (from ferricast `wayland_direct`): DMA-BUF allocated
through GBM on the device from `linux-dmabuf` feedback (tiled
modifiers, not linear), `copy_with_damage`, `ready` = rendering done
(niri), output transform → `orientation`. Buffer pool released through
the hook.

**X11** (from sss `x11`): XShm `GetImage` for monitors, Composite
`NameWindowPixmap` for windows (occluded content), XDamage for
`OnDamage` frame rate and damage rects, XFixes cursor image +
`CursorNotify`; DRI3 `BuffersFromPixmap` for DMA-BUF when the server
offers it (opt-in format request).

**KMS** (opt-in, `CAP_SYS_ADMIN`): `drmModeGetFB2` + PRIME export of
the scanout buffer; no cursor plane composition; capability notes say so.

**Windows.Graphics.Capture**: `IGraphicsCaptureItemInterop`
(`CreateForMonitor` / `CreateForWindow`), free-threaded frame pool on
the consumer-preferred D3D11 device (GPU preference = adapter LUID),
`IsCursorCaptureEnabled`, `IsBorderRequired`, `MinUpdateInterval`
(11 24H2), dirty regions (`DirtyRegionMode`, 24H2), HDR via
`R16G16B16A16Float` with scRGB colour. **DXGI duplication**:
per-output, `AcquireNextFrame` with dirty / move rects and pointer
shape (`CursorMode::Metadata` natively), `DuplicateOutput1` for HDR
formats, access-lost recovery reported as `CaptureEvent::Renegotiated`.
**GDI**: `BitBlt` / `PrintWindow(PW_RENDERFULLCONTENT)` for
screenshots only (sss `windows.rs`).

**ScreenCaptureKit**: `SCShareableContent` for listing,
`SCContentFilter` (display minus excluded apps / windows, single
window, app), `SCStreamConfiguration` (`pixelFormat` BGRA / 420v /
420f / l10r, `colorSpaceName`, `showsCursor`, `minimumFrameInterval`,
`queueDepth` = pool depth, `capturesAudio`,
`excludesCurrentProcessAudio`), frame attachments (`SCFrameStatus`
idle frames → `Damage::Unchanged`, `dirtyRects`, `contentRect`,
`scaleFactor`). `SCScreenshotManager` (14+) for screenshots,
CGWindowList below 12.3 only.

**Android MediaProjection** (Kotlin backend): consent intent through
the activity-results plumbing, foreground service started first,
`MediaProjection.Callback` registered before `createVirtualDisplay`;
`ImageReader` with `HardwareBuffer.USAGE_GPU_SAMPLED_IMAGE` →
`Image.getHardwareBuffer()` (API 28+) → `sink.submit(buffer, meta)`
and close the `Image` at once; `onCapturedContentResize` →
renegotiation event. A second mode hands the virtual display a
consumer `Surface` (e.g. a `MediaCodec` input surface) received as a
`NativeHandle`: no buffers reach Rust at all. Audio:
`AudioRecord` with `AudioPlaybackCaptureConfiguration` → direct
`ByteBuffer` submits.

**iOS**: in-app `RPScreenRecorder.startCapture` → `CMSampleBuffer` →
`sink.submit(pixelBuffer, meta)` (`.video`) and PCM for `.audioApp` /
`.audioMic`. System-wide = Broadcast Upload Extension, a separate
process with a 50 MB memory ceiling: the extension stays Swift-only (no
Rust runtime), downsizes if needed, and writes NV12 frames into a ring
of shared memory files in the App Group container, signalled over a
Unix socket there; the app-side backend maps them and submits
`Cpu` frames (one copy, unavoidable — `IOSurface` cannot cross into the
app without an XPC service the extension may not run). The capability
report says `memory: Cpu` for `scope = System`.

**Web**: `getDisplayMedia({ video: { displaySurface, cursor }, audio,
systemAudio, selfBrowserSurface, surfaceSwitching, preferCurrentTab })`,
`MediaStreamTrackProcessor` (Chromium) → `VideoFrame` backings;
elsewhere a `<video>` + `requestVideoFrameCallback` + canvas readback,
reported as `memory: Cpu`. Screenshots = first frame.

## Errors

`CaptureError` (`#[message]`, `#[non_exhaustive]`): `NoBackendCompiled`,
`BackendNotCompiled { backend, feature }`,
`BackendUnavailable { backend, reason }`, `Unsatisfiable { backend, unmet }`,
`NoBackend { tried }`, `SourceNotFound { id }`, `SourceLost`,
`ConsentRequired`, `ConsentDenied`, `ConsentRevoked`, `OsTooOld { required }`,
`SinkUnreachable` (the sink id did not resolve: consumer in another
process), `SessionNotFound`, `Protocol { backend, detail }`,
`Os { backend, operation, code }`. Each names the backend and step.

## How ferricast adapts (zero-copy)

ferricast keeps its `ScreenCapture` trait and `CapturedFrame` types;
`ferricast-capture` becomes an adapter over this plugin's Rust client
(typed shortcut, same process):

- `NativeFrame<FrameMeta>` with a `DmaBuf` backing →
  `CapturedFrame::Gpu(GpuFrame)`: `into_backing::<DmaBuf>()` moves the
  plane `OwnedFd`s into `DmaBufFd`, the returned `BufferLease` becomes
  ferricast's `BufferLease` (pool slot stays reserved while any clone
  lives), `sync` / `producer` / `orientation` / `modifier` map field by
  field.
- `HardwareBuffer` / `RetainedBuffer` / `D3d11Texture` / `IoSurface`
  backings → `CapturedFrame::Native(NativeFrame { surface })` through a
  small `NativeSurface` impl that keeps the istmo `NativeBuffer` and
  downcasts on `as_any`.
- `Cpu` backings → `RawFrame` with `Bytes::from_owner(native_buffer)`
  (no copy).
- `wall_us` feeds ferricast's segmenter (its capture clock must be wall
  clock; see its memory notes), `pts_ns` stays for A/V sync.
- Audio sink → `PipeWireAudioCapture` replacement.

## Migration plan

1. **Mechanism** (done): typed stream shortcut, frame sinks, mobile
   entry, codegen.
2. **Crate skeleton**: trait, wire types, `select.rs`, consent types,
   capability report, desktop host with the backend registry; contract
   + Kotlin / Swift goldens; `screenshot_encoded`.
3. **Linux** (verifiable here): port ferricast's portal + PipeWire
   (SyncTimeline, modifier negotiation, damage, cursor, orientation) and
   wlr-screencopy (GBM tiled allocation from dmabuf feedback), sss's
   ext-image-copy and X11, sss's multi-monitor screenshot compose. Audio
   from ferricast `pipewire_audio`.
4. **ferricast switch**: `ferricast-capture` becomes the adapter above;
   delete its own backends once the receiver/HLS tests pass on niri +
   NVIDIA.
5. **sss switch**: `sss_capture` keeps its `Capturer` API as a thin
   wrapper (screenshots through `screenshot` + sink, CPU image only
   when it saves).
6. **Windows, macOS**: WGC + DXGI + GDI (from sss), SCK + CG (from sss).
7. **Android, iOS**: Kotlin MediaProjection backend + service, Swift
   ReplayKit in-app, broadcast extension template.
8. **Web**.

Each step lands with its tests; no step leaves a platform half-wired
behind `Auto`.

## Testing

| Platform | Here (niri Wayland, RTX 3060, Linux) | Later |
|---|---|---|
| Selection, consent state machine, capability merge | unit tests with fake backends | — |
| ext-image-copy, wlr-screencopy | headless `sway` (`WLR_BACKENDS=headless`, wlroots implements both) in CI; niri on this box for real DMA-BUF + NVIDIA modifiers | — |
| Portal + PipeWire | manual run here (dialog), then restore-token replay; `xdg-desktop-portal` test backend for CI is a follow-up | GNOME / KDE VMs |
| X11 | `Xvfb` + `xdotool` windows in CI (XShm, Composite, XDamage) | — |
| KMS | manual, root, here | — |
| Windows | `cargo clippy --target x86_64-pc-windows-msvc` from Linux | real device: WGC, DXGI, HDR monitor |
| macOS / iOS | `cargo clippy --target aarch64-apple-darwin / aarch64-apple-ios`; Swift via the goldens typecheck | device + simulator (ReplayKit needs a device) |
| Android | `cargo check --target aarch64-linux-android`; Kotlin goldens `-Werror` | emulator (MediaProjection works on emulators API 34) + device |
| Web | `wasm-bindgen-test` in headless Chromium with `--use-fake-ui-for-media-stream --auto-select-desktop-capture-source` | Firefox / Safari |

Per-backend tests share one checklist: capability report snapshot,
open → N frames → close without leaks (fd count, buffer pool back to
full), consumer that never reads (pool exhausts, capture stalls, no
growth), consumer drop mid-stream, source lost, renegotiation, every
`Unsatisfiable` path names its requirement. Pixel checks render a known
pattern (wlroots headless / Xvfb) and compare a few samples.

## Open questions

1. `screenshot` result shape: sink-only plus `screenshot_encoded`, or
   also a CPU `Vec<u8>` variant over the wire for small captures?
2. Should window / application capture on GNOME / KDE fall back to the
   portal picker under `Auto` when the caller asked for a specific
   window id (it cannot honour the id)? Proposed: no — `Unsatisfiable`.
3. Region capture spanning monitors: compose on the consumer side (two
   sessions) or a plugin-side compose that copies? Proposed: only for
   screenshots, CPU, explicit `compose: true`.
4. iOS broadcast extension transport: shared-memory NV12 ring (proposed)
   vs encoding H.264 inside the extension.
5. Restore tokens: store them for the app (keyed by source) or always
   hand them to the caller? Proposed: hand them out, never store.
