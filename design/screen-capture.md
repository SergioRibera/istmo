# `istmo-screen-capture`

Design for a cross-platform screen capture plugin: screenshots and
live streams of monitors, windows, applications and regions (single
or spanning several monitors), with an honest capability report,
explicit backend and format selection, consent modelled as state, a
control plane over istmo and a data plane over native frame sinks
(`design/native-frames.md`) or straight into a platform surface.
Implemented so far: the contract, selection, pools, pixel helper,
desktop host and the Linux backends (wlr-screencopy, ext-image-copy,
X11, portal over PipeWire with DMA-BUF zero-copy, PipeWire audio). Later
steps are listed under "Implementation order"; deviations are in
"Implementation notes" below.

Per-platform facts, version tables and official sources live in
`design/screen-capture-platforms.md`; this document refers to them by
backend id.

## Implementation notes (Linux, as built)

This section records where the implementation differs from the text
below, or makes it more precise. The text below is the design; where
they disagree, this section wins.

**Wire names**
- `None`-named variants are renamed:
  - `Scaling::Unsupported`
  - `DamageSupport::Unsupported`
  - `OrientationSupport::Unsupported`
  - `Persistence::{Unsupported, Token}`
- `Requirement::Kind(SourceKind)` replaces `SourceKind(SourceKind)`. A
  variant is never named after its payload type: a Kotlin nested class
  would shadow that type.
- Caps and frames share one `SyncKind`; there is no `FrameSync`.
- `BufferIdentity.pool_generation` is a `u64`, as in core.
- `Orientation { mirrored, rotation: Rotation }` means mirror first, then
  quarter turns clockwise.
- `ProjectionGrant { handle }`.
- `BackendId` is an enum; `as_str()` is also the feature name and the
  env-override name.
- `CaptureError::NoBackend(Vec<BackendAttempt>)` carries
  `AttemptFailure`, never a nested `CaptureError`.
- Added: `CaptureError::SinkFull` (a full `DropNewest` sink refused a
  screenshot frame) and `Requirement::CompositeSize` (see Composite
  bounds).
- `AccessStatus` is defined once in `istmo-media`, shared with the
  camera plugin. It has the camera's four states plus `RequiresRestart`,
  `PerSession` and `NotApplicable`, and is `#[non_exhaustive]`.
- Kotlin and Swift keep a variant of a `#[non_exhaustive]` enum that
  they do not know (`AccessStatus`, `CaptureError`) as `Unknown` /
  `.unknown` with its discriminant and the rest of the message, and write
  it back unchanged. An older app thus reads a newer `CaptureError`
  (always the whole error payload) without failing.

**Copies.** No `CopyStage` was added: the plugin no longer makes a second
CPU copy anywhere.
- `wl_shm` frames are the pool memory itself, mapped read-only.
- X11 frames are the MIT-SHM segment of their pool slot (one segment per
  slot).
- Portal `MemFd` frames are a mapping of the PipeWire buffer.

In all three cases the slot stays leased while any clone of the frame
lives. PipeWire `MemPtr` buffers are never negotiated. The only CPU
copies left are the declared ones:
- `ShmTransport`: the producer's own copy.
- `ExhaustionReadback`: for `CopyToCpu`, the compositor copies into a
  reused `wl_shm` scratch buffer and the plugin copies that out. This
  also serves DMA-BUF sessions; a portal DMA-BUF stream has no read-back
  and is `Unsatisfiable(ExhaustionReadback)`.
- `Composite`.
- `OrientationUpright`: listed only when the source really arrives
  rotated. On an upright source, `Upright` is accepted on any memory.

**Portal.**
- DMA-BUF modifiers come from the caller's tier, intersected with the
  compositor's `linux-dmabuf` feedback (main tranche). An empty tier
  uses the compositor's modifiers. Without a Wayland display only
  `MemFd` is offered.
- Modifiers are offered `MANDATORY | DONT_FIXATE`, so the producer
  fixates one.
- Buffers stay dequeued while leased: all but one, as in
  ferricast `lease.rs`. A late release of a removed buffer is inert.
- Explicit sync: when the producer negotiates `SyncTimeline`, the
  plugin checks the acquire point through a DRM syncobj without
  blocking; one not yet signalled is waited for (at most 100 ms) on a
  per-stream thread while the buffer stays parked, so a slow producer
  never stalls the loop's other streams. The frame is then `Ready`.
  Each buffer's syncobjs are imported once. The release point is
  signalled on every re-queue, read or not (skipped older buffers,
  drains, errors, acquire timeouts), and
  `SPA_META_SYNC_TIMELINE_UNSCHEDULED_RELEASE` is cleared once it is;
  a failed signal leaves the flag set so the producer does not wait.
  Without explicit sync, DMA-BUF frames are `Implicit` (with
  `NvidiaImplicitSyncBestEffort` on NVIDIA).
- A `MemFd` is mapped only when `fstat` shows it holds
  `mapoffset + maxsize` bytes (a short one would `SIGBUS`). The
  plugin's own `wl_shm` / XShm memfds are sealed against shrinking and
  growing.
- A screenshot's `MemFd` frame is copied out
  (`CopyStage::LiveBufferSnapshot`): it holds none of the producer's
  memory once the stream ends. A DMA-BUF screenshot keeps its fd
  duplicates (no CPU readback of DMA-BUFs), as does a live frame held
  past its session; the producer frees a destroyed stream's buffers.
- The consent handshake (`SelectSources` / `Start`) is given up, and
  the portal session closed with its dialog, once the caller's future
  is dropped or after 120 s.
- Read metas: transform (into `FrameMeta.orientation`), cursor (with
  `CursorMode::Metadata`), crop, and damage (clipped).
- Verified on niri against its Mutter ScreenCast producer, without a
  dialog:
  - NVIDIA block-linear DMA-BUFs with no copies
  - a pool of 8, held at the limit with 7 out
  - frames resume once the held ones drop
- The portal stays out of `Auto`: its checklist needs a dialog per
  session, and its CPU pixel checks cannot pass on a DMA-BUF-only
  producer.

**Pools and pacing.**
- The requested pool depth is raised by the frames the pacer keeps: one
  pending frame under `Max` / `OnDamage`, plus the repeated one under
  `Exact`. Without that, `Exact` with a shallow pool starves. The
  negotiated `pool_depth` reports the raised value.
- A paced frame that arrives within 1/20 of a period of its due time
  goes out at once.
- Full copies (no damage) are paced at the output refresh.
- Paused sessions block instead of polling.
- X11 sleeps on the server socket instead of a 4 ms tick.

**Negotiated stream and events.**
- On Wayland, the first DMA-BUF frame fills `NegotiatedStream.modifier`
  and is preceded by `Renegotiated`, which also names the producer GPU.
- A size change emits `SourceResized`.
- The probe cache (5 s) is also dropped when a source vanishes, which is
  what hot-plug invalidation amounts to.
- Every `events()` subscriber first gets the session's state so far:
  `Started`, then the newest 64 events.
- Restore tokens never travel in events. They are returned once, by
  `open`.

**Security.**
- Sessions answer only the `CallOrigin` that opened them. Session ids
  are random, in the Rust-allocated range above the runtime counter.
- `events` is always local.
- A remote caller sees no titles, app ids, pids or tokens.
- An `open` whose caller gave up tears its session down, including when
  portal consent arrives late.
- Dropping a session handle never blocks: teardown runs on a thread of
  its own (never queued behind host calls in the worker pool), and the
  PipeWire loop join is bounded.
- Host calls run on a worker pool (at most 8 threads, 64 queued calls);
  a worker starts whenever every idle one is already promised to a
  queued call. `backends` and `capabilities`, open to every origin, run
  at most 4 at a time; more are refused at once.
- Wayland damage is clipped and capped at 32 rects, like X11 and the
  portal.
- Picker-only backends refuse `Region`, `Focused` and `UnderCursor`.
  For `Id` and `Monitors(ids)` the pick must be the named sources
  (`SourceNotFound` / `Unsatisfiable(SourceSelector)` otherwise; portal
  ids persist across token-restored sessions). `PrimaryMonitor` cannot be
  checked: the negotiated source never claims `primary` there.
- `wl_shm` layouts from the compositor are validated.
- GBM planes are exported with `DRM_CLOEXEC`.

**Composite bounds.**
- A composite is refused with `Unsatisfiable(CompositeSize)` above 2^28
  pixels, or when it would be more than 4 times the area of its parts.
- `CompositeScale::Permille` is clamped to 4 times the highest monitor
  scale.
- Allocation uses `try_reserve`.
- A part the CPU engine cannot read (`Rgba16f`, `Nv12`, `P010`) is
  `Unsatisfiable(Formats)` instead of rendering black.

**unsafe.** The crate denies `unsafe_code`. Two FFI modules allow it:
- `desktop/linux/mapping.rs`: `mmap`.
- `desktop/linux/portal/raw.rs`: `pw_buffer` / `spa_buffer` access.

Every unsafe block carries a `SAFETY:` comment. `undocumented_unsafe_blocks`
and `multiple_unsafe_ops_per_block` are denied. Syncobjs go through the
safe `drm` crate.

**Sources.**
- `Source` carries `make`, `model`, `connector` and `description` for
  monitors, each `None` when the platform does not say:
  - Wayland: `wl_output` geometry make / model (empty or `Unknown`
    dropped), `wl_output` / xdg-output name and description.
  - X11: the `RandR` output name of the monitor's first active output,
    and the PNP id and monitor-name descriptor of its `EDID` property
    (Xvfb and most virtual outputs have none).
- Wayland windows come from the compositor's toplevel list, ported
  from ferricast's `wlroots_enum`, on both direct backends:
  - `ext_foreign_toplevel_list_v1` first: its identifiers are stable
    and are what ext-image-copy captures (`toplevel:<identifier>`).
  - `zwlr_foreign_toplevel_manager_v1` otherwise
    (`wlr-toplevel:<protocol id>`, stable only on one connection; adds
    `minimised` and the activated window as `flags.primary`, as on X11).
  - A toplevel is listed once its first `done` arrived.
  - `Source::capture` (`SourceCapture`): `ById` where the backend
    captures it (ext-image-copy with
    `ext_foreign_toplevel_image_capture_source_manager_v1`);
    `PickerOnly(backends)` otherwise (wlr-screencopy, which captures
    outputs only; ext-image-copy without toplevel capture). The host
    fills `backends` with the ready picker-only backends offering
    windows (the portal), empty when none runs. An `Id` naming such a
    source is `Unsatisfiable(WindowById)`. `caps.source_kinds` keeps
    saying what the backend captures, not what it lists.
  - Origin rule unchanged: `sources` is local unless the policy opens
    it.
- `watch_sources(filter)` (`#[stream]`, always local): `Listed(SourceList)`
  first, then `Added(Source)`, `Changed(Source)`, `Removed(SourceId)`.
  - The backend's watcher publishes whole listings on its own thread
    (`Backend::watch`, one thread per subscriber); the host diffs them
    by id (`SourceChange::between`: removals, additions, changes) and
    names the pickers, so every backend shares the same diff and filter.
    A subscriber that leaves is noticed within 250 ms.
  - Wayland: one connection keeps the toplevel list bound and binds
    outputs plugged in later (with their xdg-output); a removed output
    leaves `outputs` too. Every relevant event bumps a counter; after one,
    two round trips let the batch (`done`, xdg-output) finish before
    listing. Outputs are listed only once described (`done`, plus the
    logical size with xdg-output), so no 0×0 monitor appears mid-plug.
  - X11: the root window's property, structure and substructure events,
    property and structure events of every listed window, and `RandR`
    screen / output / CRTC / output-property notifications. Any event
    relists after 30 ms; the diff drops no-op relistings.
  - Backends without a watcher (portal) answer
    `Unsatisfiable(Operation)`.
  - Rust sugar: `ScreenCaptureClientExt::source_watch` returns a
    `SourceWatch` that applies each change to its own list.
  - Probe-cache invalidation does not use the watch yet (the 5 s bound
    and the vanished-source drop stay).

## Goals

- One API for Linux (Wayland + X11), Windows, macOS, iOS / iPadOS,
  Android and the web, callable from Rust, Kotlin and Swift.
- Zero-copy where the platform allows it. Every copy the plugin makes
  is named (`CopyStage`) in the capability report and in the
  negotiated stream; none happens behind the caller's back.
- Nothing silent: a requirement is met or the call fails naming the
  requirement and the backend; a mid-session downgrade happens only to
  a tier the caller listed, and is announced in band.
- Report what a consumer needs to plan, and say how each fact is known
  (probed, from a table, or only after consent).
- Raw frames only. The plugin never encodes (no PNG, no video).
  Screenshots are the same frame type as streams.
- ferricast and sss become consumers without regressions
  ("ferricast adapter").

Non-goals: encoding, recording to files, input injection, overlays,
telling the user that capture is happening (the app's job; see
"Security" for what the plugin reports to help).

## Changes from the previous draft

- iOS / iPadOS 27 ScreenCaptureKit in-process is the primary iOS path;
  ReplayKit is legacy (< 27); the broadcast-extension transport is
  deferred.
- Capability report carries `Provenance` per section; facts known only
  after consent are `UnknownUntilNegotiated`; `NegotiatedStream` is
  defined field by field.
- Formats are ordered tiers with fallback, intersected with what the
  compositor offers; downgrades only to listed tiers, in band.
- Pool rule "never hand out the last free buffer" + `Exhaustion`
  policy; `BufferIdentity`; producer GPU per frame; `wall_us` sampled
  per frame; `FrameRate` semantics defined.
- Screenshots: raw frame into a sink, or raw pixels over the wire
  (`screenshot_pixels`); no PNG anywhere.
- Multi-monitor regions for screenshots **and** streams (composite).
- Restore tokens are returned to the app (redacted type), never stored
  by the plugin.
- No consent gating of `Auto` (direct Wayland, X11, WGC stay usable);
  a separate bus-origin rule keeps capture same-process by default.
- Consent folded into `open` (`allow_picker`); `request_access` only
  for system-setting grants; `AccessStatus::RequiresRestart`; Android
  grant handle.
- Preview mode: frames rendered straight into a platform surface
  (Android `Surface`, Apple `AVSampleBufferDisplayLayer`, web
  `<video>`), first-class next to sinks.
- Only wire shapes istmo's codegen accepts (no named-field enum
  variants, tuples, maps, `Duration`, generic messages,
  `Option<FrameSinkId>`).

## Crate layout

```
plugins/screen-capture/                 crate `istmo-screen-capture`, id `istmo.screen_capture`
  istmo.toml                            manifest + static permissions
  src/lib.rs                            trait, client sugar (SessionHandle, screenshot helpers)
  src/types/{source,caps,config,meta,consent,error}.rs   wire types
  src/policy.rs                         CapturePolicy (app-owned, not a wire type)
  src/select.rs                         Auto / Fixed / Ordered + format tiers, shared by every host
  src/pool.rs                           pool accounting, Exhaustion, BufferIdentity
  src/composite/{mod,vulkan,d3d11,metal,cpu}.rs                  multi-output composite
  src/pixels.rs                         CPU pixel helper (crop / orient / swizzle), no encoding
  src/desktop.rs                        Rust host: backend registry, session table, origin checks
  src/desktop/linux/{portal,pipewire,ext_image_copy,wlr_screencopy,x11,kms,gbm}.rs
  src/desktop/windows/{wgc,dxgi,gdi,d3d11}.rs
  src/desktop/macos/{sck,iosurface}.rs
  src/web.rs                            getDisplayMedia host (wasm32)
  native/android/                       ScreenCaptureBackendImpl.kt, IstmoProjectionService.kt,
                                        ScreenCaptureDefaults.kt (builders)
  native/ios/                           ScreenCaptureBackendImpl.swift (SCK 27+, ReplayKit legacy),
                                        ScreenCaptureDefaults.swift
  tests/
```

Backends are cargo features: `linux-portal`, `linux-ext-image-copy`,
`linux-wlr-screencopy`, `linux-x11`, `linux-kms`, `windows-wgc`,
`windows-dxgi`, `windows-gdi`, `macos-sck`; plus `composite-gpu`
(Vulkan / D3D11 / Metal composite). All default except `linux-kms`.
Platform dependencies are target-gated; a build with no backend for
its target answers `NoBackendCompiled` at run time instead of
`compile_error!` (ferricast's lesson: such messages drift).

## Control plane

```rust
#[istmo::plugin(name = "istmo.screen_capture", remote = true)]
pub trait ScreenCapture {
    /// Compiled backends, whether each can run here, and why not.
    async fn backends(&self) -> Vec<BackendStatus>;

    /// Capability report for every runnable backend (or one), for a
    /// source kind or a concrete source. Never prompts.
    async fn capabilities(&self, query: CapabilityQuery) -> Result<CapabilityReport, CaptureError>;

    /// Monitors, windows, applications capturable without a picker.
    /// Same origin rule as capture (titles and pids are sensitive).
    async fn sources(&self, filter: SourceFilter) -> Result<SourceList, CaptureError>;

    /// `sources`, then every change to it (local only).
    #[stream]
    fn watch_sources(&self, filter: SourceFilter) -> Result<SourceChange, CaptureError>;

    /// Standing grants (TCC, Windows borderless, Android notifications),
    /// without prompting.
    async fn access(&self, scope: AccessScope) -> Result<AccessState, CaptureError>;

    /// Prompt for a standing grant. Per-session consent (portal dialog,
    /// pickers, MediaProjection, getDisplayMedia) is never asked here:
    /// it happens inside `open` / `screenshot` when `allow_picker` is set.
    async fn request_access(&self, request: AccessRequest) -> Result<AccessState, CaptureError>;

    /// Android only: run the MediaProjection consent now and keep the
    /// grant for one later `open` / `screenshot` (see "Consent").
    async fn request_projection(&self, request: ProjectionRequest)
        -> Result<ProjectionGrant, CaptureError>;

    /// One frame (or one per monitor) into `sink`. When this returns
    /// `Ok`, `info.frames` frames are already queued in the sink.
    async fn screenshot(&self, request: ScreenshotRequest, sink: FrameSinkId<FrameMeta>)
        -> Result<ScreenshotInfo, CaptureError>;

    /// Same capture, raw pixels over the wire (Kotlin / Swift / another
    /// process). Never encoded: bytes + width / height / stride / format.
    async fn screenshot_pixels(&self, request: ScreenshotRequest, layout: PixelLayout)
        -> Result<RawImage, CaptureError>;

    /// Live session, frames into `video`.
    #[owned]
    async fn open(&self, config: SessionConfig, video: FrameSinkId<FrameMeta>)
        -> Result<CaptureSession, CaptureError>;

    /// Live session with audio into a second sink.
    #[owned]
    async fn open_with_audio(&self, config: SessionConfig, video: FrameSinkId<FrameMeta>,
                             audio: FrameSinkId<AudioMeta>) -> Result<CaptureSession, CaptureError>;

    /// Live session rendered straight into a platform surface (preview).
    #[owned]
    async fn open_preview(&self, config: SessionConfig, target: PreviewTarget)
        -> Result<CaptureSession, CaptureError>;

    /// Preview plus frames from one consent. On Android this is the GPU
    /// fan-out (`CopyStage::PreviewFanOut`); elsewhere the frames are
    /// shared with the preview without a copy where the OS allows it.
    #[owned]
    async fn open_preview_with_frames(&self, config: SessionConfig, target: PreviewTarget,
                                      video: FrameSinkId<FrameMeta>) -> Result<CaptureSession, CaptureError>;

    /// Frame rate, cursor, region, format tiers, pause / resume.
    /// Returns the renegotiated stream or an error leaving it as it was.
    async fn update(&self, session: NativeHandleId, update: SessionUpdate)
        -> Result<NegotiatedStream, CaptureError>;

    async fn close(&self, session: NativeHandleId) -> Result<(), CaptureError>;

    #[stream]
    fn events(&self, session: NativeHandleId) -> Result<CaptureEvent, CaptureError>;
}
```

`CaptureSession { session: NativeHandleId, negotiated: NegotiatedStream }`
with `#[handle(CaptureSessionTag)]`: `open_owned` returns a session
that closes on drop. Rust sugar `SessionHandle` (like camera's) bundles
session, sinks and events.

`SessionUpdate` is a tuple-variant enum: `Fps(FrameRate)`,
`Cursor(CursorRequest)`, `Region(RegionSelector)`,
`Formats(FormatRequest)`, `Pause`, `Resume`.

Why several `open` methods: `Option<FrameSinkId<_>>` is not a legal
contract shape (`ContractError::FrameSinkPlacement`), and a preview
session has no sink at all. Separate methods also let the Kotlin /
Swift client generators drop the sink-taking methods for the client
role (a Kotlin consumer cannot open a sink) while keeping
`open_preview` and `screenshot_pixels`.

`remote = true` keeps the dispatcher reachable over the bus; the
per-method origin rule is enforced by the host itself (see "Bus
origin"), so an app can open individual methods to other processes
instead of all or none.

Executor contract (matters once the typed shortcut is installed by
default): every method returns a future built on flume / oneshot that
completes on any executor; blocking platform work (portal D-Bus,
PipeWire loop, Wayland queues, WGC frame pool, SCK queues) runs on the
plugin's own threads. No method needs the "blocking" marker.

## Sources

```rust
#[istmo::message]
pub struct Source {
    pub id: SourceId,                 // opaque, backend-scoped
    pub kind: SourceKind,             // Monitor | Window | Application
    pub name: String,
    pub app_id: Option<String>,       // Wayland app_id, bundle id, AUMID, package
    pub pid: Option<u32>,
    pub geometry: Option<Rect>,       // logical, desktop global space
    pub physical_size: Option<Size>,
    pub scale_permille: Option<u32>,  // 1000 = 1.0
    pub refresh_mhz: Option<u32>,
    pub orientation: Orientation,
    pub flags: SourceFlags,           // struct of bools: primary, minimised, occluded, protected, own_process
    pub make: Option<String>,         // monitors: wl_output make, EDID PNP id
    pub model: Option<String>,        // monitors: wl_output model, EDID monitor name
    pub connector: Option<String>,    // monitors: DP-1, HDMI-A-1
    pub description: Option<String>,  // monitors: the platform's description
}
#[istmo::message] pub struct SourceId { pub backend: BackendId, pub value: String }

#[istmo::message]
pub enum SourceSelector {
    Id(SourceId),
    Region(RegionSelector),           // may span monitors
    Picker(PickerOptions),
    PrimaryMonitor,
    AllMonitors,                      // composite of every monitor
    Monitors(Vec<SourceId>),          // composite of these monitors
    Focused,
    UnderCursor,
}
#[istmo::message]
pub struct RegionSelector { pub rect: Rect, pub space: RegionSpace }   // RegionSpace: Global | MonitorRelative(SourceId)
#[istmo::message]
pub struct SourceList { pub sources: Vec<Source>, pub listing: SourceListing }
// SourceListing: Enumerable | AfterGrant (macOS before TCC) | PickerOnly (portal, iOS, Android, web)
```

A window or application requested by id on a backend that can only
offer a picker (portal, iOS, Android, web): `Unsatisfiable(WindowById)`
unless `ConsentConfig.allow_picker` is set, in which case the picker is
shown (pre-filtered to windows where the platform allows it) and the
negotiated `source` says what the person chose.

Regions are crops. A backend that crops natively (portal
`SPA_META_VideoCrop`, SCK `sourceRect`) reports `RegionCrop::Native`;
otherwise `RegionCrop::InMeta` and the crop travels in
`FrameMeta.content_rect` — no copy to crop. A region inside one
monitor is a single-output session; one crossing monitors is a
composite.

## Multi-monitor composite

Screenshots and streams both accept `AllMonitors`, `Monitors(..)` and
a `Region` that crosses monitors. The API has it from the first
release; the implementation lands streaming composite after
single-output streaming. Until then the capability report says
`composite.streaming = Unsupported` and `open` fails with
`Unsatisfiable(CompositeStreaming)` — never a silent single-monitor
session.

```rust
#[istmo::message]
pub struct CompositeConfig {
    pub scale: CompositeScale,        // HighestScale (default) | Logical | Permille(u32)
    pub fill: CompositeFill,          // Transparent (default) | Black
    pub engine: CompositeEngine,      // Auto | Gpu | Cpu
}
#[istmo::message]
pub struct CompositeInfo { pub parts: Vec<CompositePart>, pub skew_ns: u64 }
#[istmo::message]
pub struct CompositePart { pub source: SourceId, pub rect: Rect, pub pts_ns: u64, pub scale_permille: u32 }
```

Semantics:

- One capture per monitor, all on the same backend (mixing backends
  would mix clocks and consent models). Portal: one `SelectSources`
  with `multiple = true` → one dialog, one PipeWire stream per monitor.
- The composite buffer is plugin-owned and pooled like any other
  buffer. Each part is blitted upright (orientation applied during the
  blit) at the composite scale. `HighestScale` renders at the largest
  monitor scale so no monitor is downsampled; `Logical` at 1.0.
- GPU engine: Vulkan on Linux (imports the parts' DMA-BUFs, one blit
  per part into a DMA-BUF allocated with the caller's tier modifiers),
  D3D11 `CopySubresourceRegion` / video processor on Windows, Metal
  blit on macOS. CPU engine: row copies. Either way a declared copy,
  `CopyStage::Composite`.
- Streams: a composite frame is produced when any part delivers a new
  frame, at most at the session's rate, from the newest frame of every
  part. Parts hold their buffers only for the blit, so the consumer
  never exhausts the monitors' pools (only the composite pool).
  `damage` is the union of the parts' damage mapped into the
  composite; `FrameMeta.composite` lists the parts and their skew.
- Gaps between non-rectangular monitor layouts are filled
  (`Transparent` needs an alpha format; with an opaque format the fill
  is black and `NegotiatedStream.composite` says so).
- Not available on Android, iOS and the web (one display / one
  surface): `Unsatisfiable(MultiMonitor)`.
- `ScreenshotRequest.composite = false` returns one frame per monitor
  instead (`info.frames = n`, each frame's `source_id` set).

## Capability report

```rust
#[istmo::message]
pub struct CapabilityReport { pub backends: Vec<BackendCaps> }

#[istmo::message]
pub struct BackendCaps {
    pub backend: BackendId,
    pub source_kinds: Vec<SourceKind>,
    pub listing: SourceListing,
    pub consent: ConsentModel,            // honest; see "Consent"
    pub video: Vec<FormatCaps>,
    pub max_fps: Option<u32>,
    pub min_interval_us: Option<u32>,
    pub max_size: Option<Size>,
    pub scaling: Scaling,                 // None | Backend | Native
    pub region: RegionCrop,               // Native | InMeta
    pub composite: CompositeCaps,         // screenshot / streaming: Supported | Unsupported | GpuOnly | CpuOnly
    pub cursor: Vec<CursorMode>,          // Hidden | Embedded | Metadata
    pub damage: DamageSupport,            // None | Frame | Rects
    pub orientation: OrientationSupport,  // InMeta | AlreadyApplied | None
    pub colour: ColourCaps,
    pub audio: AudioCaps,
    pub producer: Option<GpuIdentity>,
    pub sync: Vec<SyncKind>,              // Ready | Implicit (Timeline never reaches consumers)
    pub persistence: Persistence,         // None | RestoreToken | SystemSetting
    pub concurrency: Option<u32>,
    pub pool: PoolCaps,                   // min / max / default depth, Exhaustion modes supported
    pub copies: Vec<CopyStage>,           // copies this backend always or conditionally makes
    pub protected_content: ProtectedContent, // Reported | MaskedUndetectable | Blank
    pub window: WindowCaptureCaps,        // occluded, minimised, child popups, border
    pub provenance: CapsProvenance,
    pub notes: Vec<CapabilityNote>,       // typed caveats, e.g. NvidiaImplicitSyncBestEffort
}

#[istmo::message]
pub struct FormatCaps {
    pub format: PixelFormat,              // Bgra8, Rgba8, Bgrx8, Xr30, Rgba16f, Nv12, P010, ...
    pub memory: BufferMemory,
    pub modifiers: Vec<u64>,              // DMA-BUF only; LINEAR only when really allocatable
    pub copies: Vec<CopyStage>,           // copies needed to deliver this entry (empty = none)
}

#[istmo::message]
pub enum Provenance { Probe, Table, UnknownUntilNegotiated }

#[istmo::message]
pub struct CapsProvenance {
    pub source_kinds: Provenance, pub listing: Provenance,
    pub video: Provenance, pub cursor: Provenance, pub damage: Provenance,
    pub colour: Provenance, pub audio: Provenance, pub producer: Provenance,
    pub sync: Provenance, pub persistence: Provenance,
}
```

Which section is which on each platform is the "Known before consent"
table in `screen-capture-platforms.md`. Rules:

- `Probe` results are cached per process and invalidated on output
  hot-plug; `Table` entries come from the platform tables by OS /
  protocol version.
- A section marked `UnknownUntilNegotiated` holds the backend's
  theoretical set (e.g. every format PipeWire might offer), never an
  empty list that reads as "nothing".
- `Auto` never rejects a backend over an `UnknownUntilNegotiated`
  section. After negotiation the requirements are checked again
  against `NegotiatedStream`; a miss ends the session with
  `Unsatisfiable { backend, unmet }` naming exactly what the chosen
  source could not do.

`backends()` returns each compiled backend with `Availability`:
`Ready`, `NotOnThisSession(String)` ("no wlr-screencopy global",
"XWayland only"), `NeedsAccess(AccessScope)`, `OsTooOld(OsVersion)`,
`NotCompiled(String)` (the feature), `PolicyDenied(String)`.

## Configuration

Wire types are plain structs. Rust gets `Default` + builders; Kotlin /
Swift get `ScreenCaptureDefaults` helpers shipped in the plugin's
native sources (generated data classes have no defaults).

```rust
#[istmo::message]
pub struct SessionConfig {
    pub source: SourceSelector,
    pub backend: BackendSelection,
    pub video: VideoConfig,
    pub audio: Option<AudioConfig>,
    pub consent: ConsentConfig,
    pub composite: CompositeConfig,
    pub linux: LinuxOptions,          // pipewire: PortalRemote (default) | SessionDaemon
    pub windows: WindowsOptions,      // borderless: Off (default) | Prefer | Require
}
#[istmo::message]
pub enum BackendSelection { Auto(AutoSelect), Fixed(BackendId), Ordered(Vec<BackendId>) }
#[istmo::message]
pub struct AutoSelect { pub prefer: AutoPreference /* ZeroCopy | LowLatency | Compatibility */, pub exclude: Vec<BackendId> }

#[istmo::message]
pub struct VideoConfig {
    pub formats: FormatRequest,
    pub fps: FrameRate,
    pub size: OutputSize,             // Native | Scaled(Size) | Fit(Size)
    pub cursor: CursorRequest,
    pub damage: Strength,             // Ignore | Prefer | Require  (damage rects)
    pub hdr: HdrPolicy,               // Sdr | PreferHdr | RequireHdr
    pub pool_depth: u8,               // 0 = backend default
    pub exhaustion: Exhaustion,
    pub gpu: GpuPreference,           // Auto | Same(GpuIdentity) | Discrete | Integrated
    pub orientation: OrientationHandling, // InMeta (default) | Upright (plugin turns; declared copy)
    pub exclude: Vec<SourceId>,       // windows / apps to leave out where the backend can (SCK)
    pub exclude_own_process: bool,
    pub live_buffers: bool,           // accept live (non-snapshot) buffers: X11 DRI3, KMS
}
#[istmo::message] pub struct CursorRequest { pub mode: CursorMode, pub strength: Strength }
```

Rust sugar: `SessionConfig::monitor(id).fps(FrameRate::Max(60)).prefer_zero_copy()`,
`SessionConfig::picker()`, `SessionConfig::region(rect)`.
`SessionConfig::default()` = primary monitor, `Auto(ZeroCopy)`, any
format, `Max(60)`, cursor embedded (prefer), damage ignored, SDR,
`Exhaustion::Stall`, `allow_picker = false`, `persist = DoNot`.

### Which fields can make a call `Unsatisfiable`

| Field | Requirement | Preference only |
|---|---|---|
| `source` | a source that cannot be captured | — |
| `backend: Fixed / Ordered` | the named backends | — |
| `formats` | union of all tiers | tier order |
| `fps` | never (`Max` / `OnDamage` are caps; `Exact` repeats frames) | — |
| `size` | `Scaled` / `Fit` when the backend cannot scale and no tier allows a copy | — |
| `cursor` | `strength = Require` | `Prefer` |
| `damage` | `Require` | `Prefer` |
| `hdr` | `RequireHdr` | `PreferHdr` |
| `pool_depth` | never (clamped, reported) | — |
| `exhaustion` | `CopyToCpu` when no readback exists for the negotiated memory | — |
| `gpu` | `Same(id)` when no listed tier allows `CrossDevice` | `Discrete` / `Integrated` |
| `source` multi-monitor | backend without composite | `composite.engine` |
| `exclude` | never (reported as not applied) | — |
| `windows.borderless` | `Require` | `Prefer` |

### Backend selection

- `Auto` walks runnable backends in platform order (below), skipping
  `exclude` and policy-denied ones, and picks the first whose `Probe` /
  `Table` facts meet every requirement; `UnknownUntilNegotiated` counts
  as "maybe". `prefer` reorders, never filters.
- `Fixed(id)` uses that backend or fails with its error; `Ordered`
  tries in order, then fails with `NoBackend(Vec<BackendAttempt>)`.
- Env override `ISTMO_SCREEN_CAPTURE_BACKEND` (`auto`, a backend id or a
  comma list) replaces `Auto` only. Under `Fixed` / `Ordered` it is
  ignored and logged once. It can name only backends the policy
  allows (KMS needs `allow_kms` whatever the environment says).
- A session never changes backend: failure ends it with `Failed` and a
  sink close reason; the app reopens.

| Platform | `Auto` order |
|---|---|
| Linux Wayland | ext-image-copy → wlr-screencopy → portal |
| Linux sandboxed (Flatpak / Snap) | portal |
| Linux X11 (no Wayland session) | X11 → portal |
| Windows | WGC → DXGI → GDI (screenshots) |
| macOS | SCK |
| iOS / iPadOS ≥ 27 | SCK (picker) |
| iOS < 27 | ReplayKit in-app (foreground only) |
| Android | MediaProjection |
| Web | getDisplayMedia |

Direct Wayland protocols come before the portal because they give
damage, explicit buffer control and no dialog; the portal wins where
they are absent (GNOME, KDE) or sandboxed. Window capture on GNOME /
KDE is portal-only (picker).

### Format tiers

```rust
#[istmo::message]
pub struct FormatRequest { pub tiers: Vec<FormatTier> }   // ordered, best first; empty = native formats, any memory
#[istmo::message]
pub struct FormatTier {
    pub choices: Vec<FormatChoice>,   // equally acceptable, in preference order
    pub allows_copy: Vec<CopyStage>,  // copies acceptable in this tier (empty = must be copy-free)
}
#[istmo::message]
pub struct FormatChoice { pub format: PixelFormat, pub memory: BufferMemory, pub modifiers: Vec<u64> /* empty = any */ }
```

- The union of all tiers is the requirement; tier order is the
  preference. There is a CPU fallback only if the caller lists a CPU
  tier.
- DMA-BUF choices are always intersected with what the producer
  offers: `linux-dmabuf` feedback tranches (wlr), session constraints
  (ext-image-copy), `EnumFormat` (PipeWire). Inside a tier the caller's
  modifier order wins; tranche order breaks ties. The implicit modifier
  (`INVALID`) is chosen only if the caller listed it.
- ferricast's mapping: tier 1 = encoder-importable ∩ Vulkan-readable
  modifiers (`consumer_modifiers`), tier 2 = Vulkan-readable, tier 3 =
  `Cpu` with `allows_copy: [ShmTransport, ExhaustionReadback]`.
- The chosen tier index is in `NegotiatedStream.tier` and in every
  `FrameMeta.tier`.
- `SessionUpdate::Formats(..)` renegotiates once the first frame
  revealed the producer GPU (hybrid laptops: the consumer picks the
  encoder on that GPU and asks again).

### Downgrades (in band, never silent)

A session moves to a lower tier only if the caller listed it, and
announces it: `CaptureEvent::Downgraded(Downgrade)` (from tier, to
tier, reason) and the new `FrameMeta.tier` on the next frame.
`DowngradeReason`: `ImportRejected` (wlr `failed` on a DMA-BUF),
`AllocationFailed`, `ModifierUnreadable`, `ProducerChanged`,
`PoolChanged`. With no listed lower tier the session ends with
`Failed(FormatLost)`. ferricast's current silent fallbacks (switching
to `wl_shm` after an allocation failure or while every buffer is
leased) become these events or the `Exhaustion` policy.

### Frame rate

```rust
#[istmo::message]
pub enum FrameRate { Native, Max(u32), Exact(u32), OnDamage(u32) }
```

- `Native`: every frame the producer delivers.
- `Max(n)`: at most `n` per second. Frames inside the period are
  deferred, not dropped blindly: the newest is delivered at the end of
  the period (ferricast's throttle timer, `throttle_window`); damage of
  skipped frames is merged into it.
- `Exact(n)`: exactly `n` per second; while the source is idle the last
  frame is repeated as a clone of the same `NativeBuffer` (shared
  backing, same `BufferIdentity`, `FrameMeta.repeat = true`). Consumers
  that take ownership (`into_backing`) must handle shared buffers.
- `OnDamage(n)`: only when content changed, at most `n` per second.

The backend throttles at the source where it can (WGC
`MinUpdateInterval` on 24H2, SCK `minimumFrameInterval`); the plugin
enforces the rest.

## Pools and exhaustion

Buffers belong to a producer pool (PipeWire buffers, GBM bos, WGC
frame pool, ImageReader images, SCK queue).

- **Never hand out the last free buffer.** In flight = frames queued in
  the sink + frames the consumer holds (clones and leases included).
  A pooled buffer is delivered only while `in_flight < pool − 1`, so the
  producer always has one to render into (ferricast `lease.rs`: "all
  but one"; a pool of one or less holds nothing).
- Pool depth negotiates from `pool_depth`; ask for
  `sink depth + frames kept + 1`. Clamped per backend (SCK 3..=8, see
  `PoolCaps`) and reported in `NegotiatedStream.pool_depth`.

```rust
#[istmo::message]
pub enum Exhaustion { Stall, DropNewest, CopyToCpu }
```

- `Stall` (default): stop pulling frames (direct protocols stop
  requesting; WGC / SCK / ImageReader frames are released at once)
  until a buffer comes back; the next delivered frame is fresh and
  carries `skipped_before`.
- `DropNewest`: keep the producer running and discard new frames until
  a buffer returns (producers that cannot pause).
- `CopyToCpu`: read the frame back into a CPU buffer and deliver that,
  lease-free, so a consumer holding `pool − 1` frames forever keeps
  receiving frames (ferricast: its pacer keeps the last frame for
  repeats). Readback goes through the GPU API, never `mmap` of a tiled
  or NVIDIA LINEAR buffer. The frame has
  `copied = Some(CopyStage::ExhaustionReadback)`.

Each exhaustion episode emits `CaptureEvent::Exhausted` once and is
counted in the session stats.

### Buffer identity

`FrameMeta.buffer: BufferIdentity { pool_generation: u32, slot: u32 }`.
Same `(generation, slot)` = same DMA-BUF object / texture / IOSurface,
so consumers cache imports (VA-API surfaces, EGL images, NVENC
registrations) by it instead of by fd number (each frame carries its
own `dup`). `pool_generation` bumps on every reallocation (size,
format, modifier, device change, DXGI access lost); consumers drop
their caches when it changes. Repeats keep the identity.

## Data plane

```rust
#[istmo::message]
pub struct FrameMeta {
    pub seq: u64,
    pub pts_ns: u64,                 // capture time on `pts_clock`
    pub pts_clock: PtsClock,         // Monotonic | Qpc | MachContinuous | HostTime | Performance
    pub wall_us: u64,                // UNIX epoch µs, sampled per frame
    pub size: Size,                  // buffer size
    pub content_rect: Rect,          // valid area (crop, letterbox)
    pub format: PixelFormat,
    pub memory: BufferMemory,
    pub planes: Vec<PlaneLayout>,    // offset, stride, size per plane
    pub modifier: Option<u64>,
    pub buffer: BufferIdentity,
    pub tier: u8,
    pub copied: Option<CopyStage>,   // this frame went through an occasional copy
    pub producer: Option<GpuIdentity>,
    pub colour: Colour,
    pub hdr: Option<HdrMetadata>,
    pub orientation: Orientation,    // still to apply (mirror, then quarter turns)
    pub damage: Damage,              // Full | Rects(Vec<Rect>) | Unchanged
    pub cursor: Option<CursorState>,
    pub sync: FrameSync,             // Ready | Implicit
    pub scale_permille: u32,
    pub source_id: SourceId,
    pub repeat: bool,
    pub skipped_before: u32,         // frames skipped by the producer side since the previous one
    pub protected_content: bool,     // where the OS reports masking
    pub composite: Option<CompositeInfo>,
}
```

- **Clocks.** `pts_ns` is the producer's own timestamp on the named
  clock (PipeWire / Wayland `CLOCK_MONOTONIC`, WGC QPC, SCK
  `mach_continuous_time`, ReplayKit host time, Android `Image`
  timestamps = `CLOCK_MONOTONIC`, web `performance.now`). `wall_us` is
  computed **per frame**: `wall_now − (clock_now − pts)`, both clocks
  sampled at delivery, so wall-clock steps and suspend show up at once
  (ferricast's segmenter compares against `SystemTime::now()`; a
  one-off offset drifts).
- **Sync.** `Ready`: writing finished (explicit acquire waited by the
  plugin; wlr / ext `ready` on a compositor known to wait; SCK, WGC,
  Android after their fences). `Implicit`: DMA-BUF implicit fences,
  best effort on NVIDIA (capability note). Dropping the frame / lease
  means every consumer GPU read is complete; the release hook signals
  the producer's release point.
- **Meta vs backing.** `planes`, `modifier` and `format` describe the
  same buffer as the backing; the backing is authoritative for handles
  (fds, pointers), the meta lets the consumer plan before touching the
  buffer.

### `BufferMemory` (plugin-owned, wire-encodable)

```rust
#[istmo::message]
pub enum BufferMemory { Cpu, DmaBuf, D3d11Texture, IoSurface, CvPixelBuffer, HardwareBuffer, WebVideoFrame }
```

Separate from `istmo_core::native_frame::MemoryKind` (not a wire type,
no web variant); `From<MemoryKind>` maps the overlap. The different
name avoids a clash when both crates are glob-imported.

Backings: Linux core `DmaBuf` (or `from_cpu` for shm / MemFd);
Windows plugin `D3d11Frame` (`ID3D11Texture2D` + device + optional NT
shared handle); macOS plugin `IoSurfaceFrame` (retained `IOSurface` +
`CVPixelBuffer`); iOS `istmo_ios::RetainedBuffer`; Android
`istmo_android::HardwareBuffer` with the `Image` lease; web
`WebVideoFrame` (closes the JS `VideoFrame` on drop).

### Copies (`CopyStage`)

```rust
#[istmo::message]
pub enum CopyStage {
    DxgiMandatory,        // DXGI: one acquired frame at a time → CopyResource into the pool
    CrossDevice,          // texture copied into a shared texture on the consumer's device
    FormatConversion,     // GPU colour conversion (e.g. WGC BGRA → NV12)
    Composite,            // multi-monitor blit
    OrientationUpright,   // `orientation: Upright` requested
    ExhaustionReadback,   // `Exhaustion::CopyToCpu`
    ShmTransport,         // wl_shm / MemFd / XShm: producer copied into CPU memory
    LiveBufferSnapshot,   // X11 DRI3 / KMS live buffer copied so it stops changing
    WebCopyTo,            // VideoFrame.copyTo into wasm memory
    PreviewFanOut,        // Android: one capture drawn into the preview surface and the sink
    LegacyRing,           // iOS legacy extension → app ring
}
```

`NegotiatedStream.copies` lists the copies made on every frame;
`FrameMeta.copied` marks occasional ones.

### Audio

`open_with_audio` feeds `FrameSink<AudioMeta>` with `Cpu` buffers
(interleaved PCM):

```rust
#[istmo::message]
pub struct AudioConfig { pub source: AudioSource, pub rate: Option<u32>, pub channels: Option<u8>, pub format: SampleFormat }
#[istmo::message]
pub enum AudioSource { System, Application(String), ExcludeSelf, Node(u32) }
#[istmo::message]
pub struct AudioMeta { pub seq: u64, pub pts_ns: u64, pub wall_us: u64, pub rate: u32, pub channels: u8,
                       pub format: SampleFormat, pub frames: u32, pub discontinuity: bool }
```

- `Node(u32)`: a PipeWire node id (ferricast's monitor-node path).
- Queue policy is the caller's sink policy; the plugin prescribes
  none. ferricast uses `queue(32)` and treats any drop as a fault (it
  sees them in `dropped_before` / `SinkStats`).
- `wall_us` follows the sample count from an anchor and re-anchors on
  a gap or drift beyond a threshold (`discontinuity = true`), ported
  together with ferricast's `pipewire_audio` clock tests.
- Sources: PipeWire monitor / app nodes; WASAPI loopback (process
  loopback from build 20348); SCK `capturesAudio`; Android
  `AudioPlaybackCaptureConfiguration` (may be silent:
  `AudioCaps.may_be_silent`); iOS SCK / ReplayKit `.audioApp`;
  `getDisplayMedia` audio.

## Preview into a platform surface

For consumers that only show the capture (mobile apps, a Kotlin /
Swift UI), frames go from the OS straight into a surface the app owns;
no buffer reaches Rust.

```rust
#[istmo::message]
pub struct PreviewTarget { pub surface: NativeHandleId, pub kind: PreviewSurfaceKind, pub fit: PreviewFit }
// PreviewSurfaceKind: AndroidSurface | AvSampleBufferDisplayLayer | HtmlVideoElement
// PreviewFit: Fit | Fill | Stretch
```

The surface is an app object registered in the runtime's handle table
(mechanism item 13): Kotlin
`IstmoRuntime.registerObject(surfaceView.holder.surface)`, Swift
`IstmoRuntime.register(object: layer)`, wasm a JS element, Rust on
Android an `ANativeWindow`. Only same-process hosts resolve it.

| Platform | How | Copies | Frames to Rust as well |
|---|---|---|---|
| Android | VirtualDisplay renders into the `Surface` (SurfaceView, TextureView, or a `MediaCodec` input surface) | none | yes, through the GPU fan-out below (one `CopyStage::PreviewFanOut` per frame) |
| iOS 27 / macOS | SCK sample buffers enqueued into `AVSampleBufferDisplayLayer` | none (retain) | possible; a combined method is added when a consumer needs it |
| Web | `video.srcObject = stream` | none | possible (`MediaStreamTrackProcessor` alongside), same note |
| Linux / Windows | not offered: desktop consumers render from sinks (DMA-BUF / D3D11 import) | — | — |

Resize, rotation and letterboxing are handled by the backend
(`onCapturedContentResize` → VirtualDisplay resize; layer
`videoGravity` from `PreviewFit`). Events (`SourceResized`,
`ConsentRevoked`, `Stopped`) are the same as for sink sessions.

The `MediaCodec` input surface is also the best Android encode path
for ferricast: capture to encoder with no buffer leaving the GPU.

### Preview and frames together on Android

One consent gives one VirtualDisplay, and a VirtualDisplay renders into
one surface. Two ways to show a preview while Rust also gets frames:

- **Fan-out (the plugin does it):** `open_preview_with_frames(config,
  target, sink)` makes the VirtualDisplay render into a plugin-owned
  `SurfaceTexture`; on each frame the plugin draws that texture into
  the preview surface and into the sink's `ImageReader` surface with
  EGL / GLES. One GPU copy per frame per extra output (well under 1 ms
  on current phones, some battery), declared as
  `CopyStage::PreviewFanOut` in `NegotiatedStream.copies`. Works with
  no app-side rendering.
- **The app renders its own preview (recommended when it already
  draws):** open a sink session only and draw the received
  `AHardwareBuffer` frames in the app's own renderer (e.g. Freya
  importing them through Vulkan). No extra copy: the app renders those
  frames anyway. This is ferricast's path.

Asking for both a preview target and a sink without the fan-out method
stays an error (`Unsatisfiable(PreviewAndFrames)`), so the copy is
never added silently.

## Screenshots

```rust
#[istmo::message]
pub struct ScreenshotRequest {
    pub source: SourceSelector,
    pub backend: BackendSelection,
    pub formats: FormatRequest,     // empty = CPU tier first (see guarantees)
    pub cursor: CursorRequest,
    pub hdr: HdrPolicy,
    pub composite: bool,            // several monitors: one composite (default) or one frame each
    pub composite_config: CompositeConfig,
    pub consent: ConsentConfig,
}
#[istmo::message]
pub struct ScreenshotInfo { pub frames: u32, pub backend: BackendId, pub negotiated: NegotiatedStream }

#[istmo::message]
pub struct PixelLayout { pub format: CpuPixelFormat /* Rgba8 | Bgra8 | Rgba16f */, pub upright: bool, pub tight: bool }
#[istmo::message]
pub struct RawImage { pub width: u32, pub height: u32, pub stride: u32, pub format: CpuPixelFormat,
                      pub colour: Colour, pub source: SourceId, pub bytes: Vec<u8> }
```

Guarantees:

- `screenshot` returns `Ok` only after `info.frames` frames are queued
  in the sink (so `sink.try_recv()` right after succeeds). The frame is
  the streaming `FrameMeta` + `NativeBuffer`, same fields, same rules.
  A `latest()` sink holding an older frame has it replaced.
- `screenshot_pixels` and the Rust helper
  `client.screenshot_cpu(&rt, request, layout) -> RawImage` return raw
  pixels: cropped to `content_rect`, upright when `layout.upright`
  (default true), in the requested byte order, rows `stride` apart
  (`tight` → `stride = width × bytes per pixel`), colour described.
  The plugin never tone-maps: SDR output asks the OS for SDR
  (`HdrPolicy::Sdr`); an HDR source read as `Rgba16f` keeps its
  values. Nothing is encoded.
- The CPU path asks backends for CPU memory first (wl_shm, MemFd,
  XShm, WGC staging map, IOSurface lock, Android `RGBA_8888` reader),
  so it never `mmap`s a GPU-tiled or NVIDIA LINEAR DMA-BUF. A GPU-only
  backend is read back through its GPU API (marked in `copied`).
- Web: every call needs a user gesture and shows the picker unless an
  open session is reused (`SessionHandle::grab_frame`).

## Consent and access

Per-session consent happens inside `open` / `screenshot`;
`request_access` is only for standing, system-setting grants.

```rust
#[istmo::message]
pub enum AccessScope { ScreenRecording, BorderlessCapture, Notifications, Microphone }
#[istmo::message]
pub struct AccessRequest { pub scope: AccessScope }
#[istmo::message]
pub struct AccessState {
    pub scope: AccessScope,
    pub status: AccessStatus,
    pub model: ConsentModel,
    pub persistence: Persistence,
    pub revocable_mid_session: bool,
    pub indicator: IndicatorState,  // Shown | Hidden(String) | NotApplicable: what the OS shows the user
}
#[istmo::message]
pub enum AccessStatus { Granted, Denied, NotDetermined, Restricted, RequiresRestart, PerSession, NotApplicable }
#[istmo::message]
pub enum ConsentModel { NoneNeeded, SystemSetting, PerSessionDialog, PickerPerSession, PerSessionSystemAlert }

#[istmo::message]
pub struct ConsentConfig {
    pub allow_picker: bool,                 // false → ConsentRequired / Unsatisfiable instead of UI
    pub restore_token: Option<RestoreToken>,
    pub persist: PersistMode,               // DoNot (default) | WhileRunning | UntilRevoked
    pub grant: Option<ProjectionGrant>,     // Android: from request_projection
}
```

| Platform | Model | Standing grant (`request_access`) | Persistence | Revocation |
|---|---|---|---|---|
| Wayland portal | `PerSessionDialog` | — | restore token (portal v4+), single use | `SourceLost` / `ConsentRevoked` |
| Wayland direct, X11, KMS | `NoneNeeded` | — | — | — |
| Windows WGC / DXGI | `NoneNeeded` (yellow border drawn by the OS for WGC) | `BorderlessCapture` (20348+, packaged) | system setting | — |
| macOS SCK | `SystemSetting` (TCC), picker optional | `ScreenRecording`, may answer `RequiresRestart` | system setting; monthly re-prompt on 15 | `didStopWithError` |
| iOS 27 SCK | `PickerPerSession` | — (`PerSession`) | none | system stop |
| iOS < 27 ReplayKit | `PerSessionSystemAlert` | — | none | Control Centre stop |
| Android | `PerSessionDialog` | `Notifications` (33+, FGS notification visibility) | none | `onStop` (chip, lock, another projection) |
| Web | `PickerPerSession` (user gesture) | — | none | track `ended` |

`RequiresRestart`: macOS applies a Screen Recording grant only to a
relaunched process; the app shows its own "restart" prompt.

### Android grant handle

`request_projection` runs the consent intent now and returns
`ProjectionGrant(NativeHandleId)`. The `MediaProjection` it stands for
is held by the backend at process scope (survives Activity
recreation, e.g. rotation), is single-use (consumed by the next `open`
/ `screenshot` naming it), and dies on the projection's `onStop` or
after a timeout (`GrantExpired`). Without a grant, `open` with
`allow_picker = true` runs the consent itself; with neither, it fails
`ConsentRequired`. The consent `Intent` and the projection never cross
the bus (only the handle id does), and the handle resolves only for
same-process callers.

### Restore tokens

```rust
#[istmo::message]
pub struct RestoreToken {
    #[redact] pub value: String,   // Debug prints <redacted>; no Display
    pub backend: BackendId,
    pub plugin_version: u32,
}
```

- Returned in one place: `NegotiatedStream.restore_token` (also inside
  `CaptureEvent::Renegotiated`), only to same-process callers; a remote
  caller gets `None`.
- Default `persist = DoNot`. The app stores the token and passes it
  back in `ConsentConfig.restore_token`. Portal tokens are single-use,
  so the app replaces the stored token with the new one after every
  session.
- A token from another backend or plugin version is ignored (the
  dialog shows, `NegotiatedStream.restored = false`).
- **Storage guidance for apps**: keep it in the platform secret store
  (Secret Service, e.g. through `oo7`, which uses the Secret portal in
  Flatpak; Keychain; Windows Credential Manager), keyed by what the user
  picked; never in logs, plain config files or analytics; delete it
  when the user turns "remember my choice" off. The token skips the
  dialog, so it is as sensitive as the consent itself.

## Security

Two separate concerns.

### 1. Bus origin (istmo's bus security)

Capture is a capability, and istmo's bus can carry calls from other
processes (`CallOrigin::Remote`). Default: every capture method
(`open*`, `screenshot*`, `sources`, `request_projection`, `update`,
`close`, `events`) answers only callers in this process (`Local` =
Rust, `Peer` = the same app's Kotlin / Swift); `OriginRefused`
otherwise. `backends`, `capabilities` and `access` are open to all.
The app opens specific methods to remote callers through
`CapturePolicy.remote_methods`; restore tokens, grants and sink-taking
methods stay same-process regardless (sinks are process-local anyway).
The host reads the caller's origin from the host context (mechanism
item 9); `remote = true` on the plugin only makes a per-method check
possible instead of refusing the whole dispatcher.

### 2. Who may capture what (the app's decision)

The plugin does not gate capture by consent model: direct Wayland
protocols, X11 and WGC stay in `Auto` exactly as today (ferricast's
previews depend on them). Telling the user that capture runs is the
app's responsibility. The plugin makes that easy by being honest:

- `ConsentModel` per backend in the capability report, and
  `NegotiatedStream.consent` / `indicator` per session, so an app that
  wants consent-backed capture excludes `NoneNeeded` backends itself.
- `CaptureEvent::Started` / `Stopped` on every session, for the app's
  own indicator.

### `CapturePolicy` (app-owned, not settable over the bus)

```rust
pub struct CapturePolicy {
    pub remote_methods: Vec<&'static str>,        // methods opened to CallOrigin::Remote; default none
    pub allowed_backends: Option<Vec<BackendId>>, // None = every compiled backend
    pub allow_kms: bool,                          // default false
    pub allow_borderless: bool,                   // default false
}
```

Rust desktop host: `DesktopCapture::with_policy(policy)` in
`runtime!(hosts: …)`. Android / iOS: `ScreenCaptureConfig.policy` set
in native code before the runtime starts, frozen at first use. Not a
wire type; no method changes it.

### Specific rules

- **Borderless WGC**: `WindowsOptions.borderless` (`Off` default,
  `Prefer`, `Require`) needs `allow_borderless`, then
  `RequestAccessAsync(Borderless)` (the OS prompt). A denial keeps the
  border (`NegotiatedStream.border = Shown`; `Require` →
  `ConsentDenied`). Kept opt-in because it needs its own OS consent.
- **KMS**: cargo feature `linux-kms` **and** `allow_kms`; capability
  note `KmsBypassesCompositor`.
- **Protected content is never bypassed**: nothing works around
  `FLAG_SECURE`, `NSWindow.sharingType = .none`,
  `WDA_EXCLUDEFROMCAPTURE` or DRM. Where the OS reports masking
  (`SourceFlags.protected`, DXGI) frames carry `protected_content`;
  where it does not (Android, WGC) the report says
  `MaskedUndetectable`.
- **The app's own sensitive windows**: `exclude_own_process` and
  `exclude` are applied natively where possible (SCK excluded apps /
  windows, process-excluding loopback audio); elsewhere reported as not
  applied. Apps mark their secrets with the platform flags above.
- **Android**: `IstmoProjectionService` `exported="false"`; consent
  intents, projections and grants never on the bus; FGS started before
  `getMediaProjection`; `POST_NOTIFICATIONS` denied →
  `indicator = Hidden("notification permission denied")` in
  `AccessState` / `NegotiatedStream` (the status-bar chip on 15 QPR1+
  still shows).
- **Env override** limited to policy-allowed backends.
- **File descriptors**: every fd the plugin creates or receives is
  `CLOEXEC` (`F_DUPFD_CLOEXEC`, `DRM_CLOEXEC`).
- **iOS legacy ring** (only if the deferred extension transport is
  built): files in the App Group container with
  `NSFileProtectionComplete`, excluded from backup, zeroed and unlinked
  on stop; every header field (sizes, offsets, strides, sequence)
  bounds-checked as untrusted; frames copied out under a seqlock, and
  that copy is the submit (`CopyStage::LegacyRing`).

## Negotiated stream

```rust
#[istmo::message]
pub struct NegotiatedStream {
    pub backend: BackendId,
    pub source: Source,                  // what was actually captured (picker choice)
    pub size: Size,
    pub content_rect: Rect,
    pub format: PixelFormat,
    pub memory: BufferMemory,
    pub modifier: Option<u64>,
    pub tier: u8,
    pub copies: Vec<CopyStage>,
    pub fps: FrameRate,                  // as enforced
    pub source_rate_mhz: Option<u32>,    // what the producer delivers
    pub producer: Option<GpuIdentity>,   // None until known (portal: first frame)
    pub sync: SyncKind,
    pub pool_depth: u8,
    pub exhaustion: Exhaustion,
    pub orientation: OrientationSupport,
    pub crop: RegionCrop,
    pub damage: DamageSupport,
    pub cursor: CursorMode,
    pub colour: Colour,
    pub hdr: bool,
    pub composite: Option<CompositeInfo>,
    pub consent: ConsentModel,
    pub indicator: IndicatorState,
    pub border: BorderState,             // Windows: Shown | Hidden | NotApplicable
    pub restore_token: Option<RestoreToken>,
    pub restored: bool,
    pub audio: Option<AudioNegotiated>,
}
```

Returned by `open*`, `update`, and in `CaptureEvent::Renegotiated`.
Facts known only with the first frame (producer GPU on the portal)
arrive in a `Renegotiated` event delivered before that frame.

## Events and session end

```rust
#[istmo::message]
pub enum CaptureEvent {
    Started(NegotiatedStream),
    Renegotiated(NegotiatedStream),
    Downgraded(Downgrade),
    SourceResized(Size),
    SourceHidden,
    SourceVisible,
    Paused(PauseReason),          // Backgrounded | SecureDesktop | SystemPaused | Requested
    Resumed,
    Exhausted(ExhaustionReport),
    ConsentRevoked,
    SourceLost,
    Failed(CaptureError),
    Stopped(StopReason),          // Closed | ConsentRevoked | SourceLost | Failed | Shutdown
}
```

When a session ends, the plugin closes its sinks with the reason
(`close_with`), so a consumer that only reads the sink learns why
(`FrameSink::end_reason()`), and emits `Stopped`. Queued frames are
delivered first.

## Errors

`CaptureError` (`#[message]`, `#[non_exhaustive]`, unit / tuple
variants only): `NoBackendCompiled`, `BackendNotCompiled(BackendFeature)`,
`BackendUnavailable(BackendReason)`, `Unsatisfiable(Unmet)` (backend +
`Vec<Requirement>`), `NoBackend(Vec<BackendAttempt>)`,
`SourceNotFound(SourceId)`, `SourceLost`, `ConsentRequired`,
`ConsentDenied`, `ConsentRevoked`, `AccessRequiresRestart`,
`OsTooOld(OsVersion)`, `OriginRefused`, `PolicyDenied(String)`,
`SinkUnreachable`, `SessionNotFound`, `GrantExpired`, `FormatLost`,
`Protocol(BackendDetail)`, `Os(OsFailure)` (backend, operation, code).
Each names the backend and the step.

## Required from the frame-sink mechanism

Items this design depends on. 1–12 are already being done; 13–15 are
new.

1. Random, unguessable sink ids with origin checks: generated
   dispatchers reject `FrameSinkId` args from non-`Local` callers; the
   generated client refuses to send them to remote plugins.
2. One producer count per sink across `frame_sender` resolutions (a
   probe or a second producer must not close a live sink).
3. `FrameSink::end_reason()` and producer `close_with(error)`.
4. `BufferIdentity { pool_generation, slot }` on the backing or in the
   frame; `DmaBufPlane.size`; optionally shared planes (`Arc<OwnedFd>`).
5. No `FrameSync::Timeline` in the data plane.
6. `BufferLease: Sync`.
7. Android ticket / lease release: Kotlin keeps the `Image` open until
   Rust releases the frame (drained ticket queue, no JNI from Rust
   threads); `Image.getFence`.
8. `FrameSink` as a futures `Stream`.
9. Host context `ctx.frame_sender(sink)` **and `ctx.origin()`**: the
   per-method origin rule needs the caller's `CallOrigin`.
10. Contract validation in `#[plugin]` with spanned errors; client
    generators drop sink-taking methods for Kotlin / Swift clients.
11. Typed shortcut auto-installed by `runtime!(hosts:)`.
12. `catch_unwind` around backing drop and hooks; `CLOEXEC` on fds.
13. **App-object handles**: Kotlin / Swift / Rust-on-Android / wasm
    register a platform object (`Surface`, `AVSampleBufferDisplayLayer`,
    `ANativeWindow`, a JS element) in the runtime's handle table and get
    a `NativeHandleId` that only same-process hosts resolve, released
    when the app drops it. Needed for `PreviewTarget` and
    `ProjectionGrant`.
14. **wasm32 `!Send` backings**: a `VideoFrame` backing on the
    single-threaded wasm target (e.g. a `LocalBacking` accepted only on
    `wasm32` without atomics). Until it exists, the web host delivers
    `WebCopyTo` CPU frames.
15. **Thread-bound release**: hooks that must act on a given thread
    (PipeWire requeue on its loop, SCK / WGC objects) post and return;
    documented that hooks run on whatever thread drops the last
    reference.

Not required: `Option<FrameSinkId>`, named-field variants, tuples,
`Duration`, generic messages (the design avoids all of them).

## ferricast adapter

ferricast keeps its `ScreenCapture` trait, `CapturedFrame`, `GpuFrame`
and encoder code; `ferricast-capture` gains a feature `istmo-capture`
implementing its trait over this plugin's Rust client (typed shortcut,
same process). Its own backends stay until the guards below pass on
the adapter, then go.

### Config mapping

| ferricast `CaptureConfig` / behaviour | `SessionConfig` |
|---|---|
| `fps` | `FrameRate::Max(fps)` (throttle-and-flush) |
| `width` / `height` | `OutputSize::Fit` (backend scaling) or `Native` + encoder fit |
| `show_cursor` | `CursorRequest { Embedded, Prefer }` / `Hidden` |
| `consumer_modifiers` | tier 1 choices |
| Vulkan importer readable modifiers | tier 2 choices |
| shm / mmap fallbacks | tier 3 `Cpu`, `allows_copy: [ShmTransport]` |
| `consumer_orients` | `orientation: InMeta` when true, `Upright` otherwise |
| readback at the hold limit | `Exhaustion::CopyToCpu` |
| `Context::connect(None)` | `linux.pipewire = SessionDaemon` |
| GPU preference | `VideoConfig.gpu` |
| `PipeWireAudioCapture` | `open_with_audio`, `AudioSource::Node(id)` / `System`, sink `queue(32)` |

### Frame mapping

| istmo frame | ferricast |
|---|---|
| `DmaBuf` backing | `into_backing::<DmaBuf>()` → plane `OwnedFd` → `DmaBufFd` (moved, no dup); `BufferLease` → `ferricast_core::BufferLease::new(lease)` → `CapturedFrame::Gpu(GpuFrame)` |
| `planes[0]` offset / stride / size, `modifier`, `format` | `DmaBufPlane { offset, stride, modifier, size }`, `PixelFormat` |
| `producer: GpuIdentity::Drm(dev_t)` | `GpuFrame.producer: DrmDevice` |
| `sync: Ready / Implicit` | `FrameSync::Ready / Implicit` |
| `orientation` | `ferricast_core::Orientation` (same mirror + quarter-turn model) |
| `buffer: BufferIdentity` | import-cache key next to `DmaBufId`; caches dropped on generation change |
| `wall_us` | `timestamp_us` (segmenter wall clock) |
| `pts_ns` | A/V sync |
| `repeat` | the pacer's repeats (same lease either way) |
| `Cpu` backing | `RawFrame` with `Bytes::from_owner(native_buffer)` |
| `IoSurfaceFrame` / `HardwareBuffer` / `D3d11Frame` | `CapturedFrame::Native(NativeFrame { surface })` via a small `NativeSurface` impl holding the istmo buffer |
| importer | ferricast attaches its own Vulkan `DmaBufImporter` (the plugin has none to give) |

Native format gaps: ferricast-codec's `PixelBuffer` takes only
biplanar 4:2:0, its `HardwareBuffer` path only YUV, D3D11 no FP16. The
adapter asks SCK for `420v`; Android gives RGBA only (ferricast extends
its `HardwareBuffer` path to RGBA, or takes the preview-target route
into a `MediaCodec` input surface); WGC gives BGRA / FP16 only
(ferricast accepts BGRA, or lists a tier allowing `FormatConversion`).

### Zero-copy sketch (wlr-screencopy → NVENC on this machine)

1. The adapter opens a sink `queue(3)` and calls `open` with tiers
   [NVENC EGL modifiers] ⊃ [Vulkan-readable] ⊃ [Cpu], `CopyToCpu`.
2. The wlr backend intersects tier 1 with the feedback tranche of
   niri's main device (`0x0300000000606014` block-linear qualifies),
   allocates the GBM pool (generation 1), requests `copy_with_damage`.
3. On `ready` it pushes `NativeBuffer::from_dmabuf(dup of the slot fd)`
   with a release hook returning the slot, and `FrameMeta { tier: 0,
   sync: Ready, buffer: (1, slot), producer: Drm(dev) }`.
4. The adapter's `into_backing` moves the fd into `DmaBufFd`, wraps the
   lease; NVENC finds `(1, slot)` in its registration cache, encodes
   from the DMA-BUF and drops the frame after locking the picture →
   hook → slot free.
5. CPU bytes touched: none. `NegotiatedStream.copies`: empty. Target:
   ferricast's measured 601/601 GPU frames, 0 readbacks.

### ferricast guards that must keep passing

Ported into the plugin, or kept in ferricast and run against the
adapter:

- PipeWire lease: `holds_all_but_one_buffer`,
  `tiny_or_unknown_pool_holds_nothing`, `release_returns_each_hold_once`,
  `removed_buffers_make_late_releases_inert`,
  `shrinking_pool_lowers_the_limit_below_current_holds`.
- PipeWire format / stream / sync: `consumer_modifiers_are_offered_first`,
  `explicit_sync_buffers_param_differs_from_the_plain_one`,
  `throttle_window`, `unconnected_after_connect_and_errors_are_fatal`,
  `frame_range_takes_full_strides_when_available`,
  `frame_range_accepts_trimmed_last_row`,
  `frame_range_rejects_short_or_inconsistent_planes`; syncobj wait /
  signal / sync-file import and pod order.
- wlr pool, layout, orientation: `unleased_buffer_is_reused`,
  `leased_buffer_is_skipped_until_released`,
  `lease_is_held_by_every_frame_clone`,
  `exhausted_at_cap_when_everything_is_leased`,
  `release_from_another_thread_frees_the_slot`,
  `key_change_drops_free_and_retires_leased_buffers`,
  `discard_removes_only_unleased_buffers`, `stale_index_is_rejected`,
  `layout_prefers_what_every_side_takes`,
  `layout_without_importer_is_linear_only`,
  `layout_without_feedback_never_picks_the_implicit_modifier`,
  `identity_orientation_keeps_frame_untouched`,
  `y_invert_flips_rows_and_drops_stride_padding`,
  `rotation_by_90_swaps_axes`, `short_buffer_is_emitted_unoriented`,
  `frame_orientation_matches_the_image_path`.
- Audio clock: `first_chunk_anchors_to_now`,
  `steady_stream_follows_sample_count_not_jittery_wall`,
  `gap_reanchors_to_now`, `counted_clock_ahead_of_wall_reanchors`,
  `drift_within_threshold_is_tolerated`.
- ferricast core, unchanged, must hold through the adapter:
  `dmabuf_fd_dup_outlives_source_and_keeps_identity`,
  `lease_returns_buffer_once_after_last_clone`,
  `into_cpu_holds_lease_until_readback_done`,
  `into_cpu_turns_a_rotated_frame`, the orientation tests; Vulkan
  `dupd_fds_share_buffer_id`, `env_override_beats_compositor_main_device`,
  `device_rank_prefers_drm_match_then_integrated`.
- Integration: all of `tests/nvidia_e2e.rs` (notably
  `nvenc_h264_encodes_tiled_dma_bufs_zero_copy`,
  `nvenc_h264_encodes_linear_dma_bufs_zero_copy`,
  `odd_sized_frames_go_zero_copy_and_refused_buffers_stay_read_back`,
  `a_zero_copy_failure_falls_back_for_that_frame_only`,
  `nvenc_turns_rotated_dma_bufs_on_the_gpu`,
  `nvenc_turns_and_letterboxes_flipped_dma_bufs_on_the_gpu`),
  `tests/gpu_preference.rs`, `tests/receiver_e2e.rs`, `tests/churn.rs`,
  `tests/nvidia_throughput.rs`.
- Live checks from ferricast's NVCODEC.md "Capture zero-copy", repeated
  on the adapter: DP-3 every frame 601/601 GPU frames with 0 readbacks;
  idle screen with damage + pacing = 1 capture and repeats; rotated
  outputs 0 readbacks with GPU rotation; Vulkan readback bit-exact
  against `grim`; timestamps on the wall clock (HLS segmenter keeps
  the audio).

## Testing

What this machine (niri 26.04 Wayland, NVIDIA RTX 3060, Linux) can
verify, and what needs other hardware:

| Area | Here | Elsewhere |
|---|---|---|
| Selection, tiers, provenance, pool / exhaustion accounting, composite layout maths, pixel helper, policy / origin | unit tests with fake backends | — |
| wlr-screencopy | niri: real DMA-BUF, NVIDIA modifiers, damage, rotated outputs | CI: headless sway (`WLR_BACKENDS=headless`, mostly shm) |
| ext-image-copy | **not on niri 26.04** | CI: headless sway 1.11; manual on Hyprland / COSMIC |
| Portal + PipeWire | niri's portal, manual (dialog): restore-token replay, multi-monitor composite | GNOME / KDE VMs; an explicit-sync producer once Mutter / KWin merge it |
| X11 | Xvfb + xdotool in CI (XShm, Composite, XDamage, XFixes) | DRI3 on a real Xorg |
| KMS | manual, root, here | — |
| GPU composite (Vulkan) | here, NVIDIA | Mesa iGPU |
| ferricast adapter | here: nvidia_e2e, receiver_e2e, live `mycap` runs | — |
| Windows | `cargo clippy --target x86_64-pc-windows-msvc` | device: WGC, DXGI, borderless prompt, HDR, multi-GPU |
| macOS | `cargo clippy --target aarch64-apple-darwin`; Swift goldens typecheck | Mac: TCC flow and restart, SCK formats |
| iOS | `cargo clippy --target aarch64-apple-ios`; Swift goldens (swiftc 5.10 here; SCK needs the iOS 27 SDK) | iOS 27 device for SCK; < 27 device for ReplayKit |
| Android | `cargo check --target aarch64-linux-android`; Kotlin goldens `-Werror` | emulator API 34+ and a device (HardwareBuffer, 15 QPR1 chip / lock stop) |
| Web | `wasm-bindgen-test` in headless Chromium with `--use-fake-ui-for-media-stream --auto-select-desktop-capture-source` | Firefox, Safari |

Per-backend checklist: capability snapshot with provenance; open → N
frames → close with no leaks (fd count, pool back to full); consumer
never reads (no growth, one `Exhausted`); consumer holds `pool − 1`
forever (`CopyToCpu` keeps frames flowing; `Stall` resumes on
release); sink dropped mid-stream; source lost; renegotiation bumps
`pool_generation`; downgrades only to listed tiers; every
`Unsatisfiable` names its requirement; remote origin refused; token
absent from `Debug` output. Pixel checks: known pattern (headless sway
/ Xvfb) sampled after crop, orientation and composite.

## Implementation order

1. **Skeleton**: wire types, `select.rs`, pool accounting, pixel
   helper, desktop host with policy and origin checks, contract and
   Kotlin / Swift goldens; fake-backend tests.
2. **Linux**: wlr-screencopy and portal + PipeWire (ports of
   ferricast), then X11 and ext-image-copy (ports of sss) with headless
   sway + Xvfb CI; screenshots including composite; audio.
3. **ferricast switch** behind `ferricast-capture/istmo-capture`;
   guards above pass; then ferricast's own backends go.
4. **sss switch**: `sss_capture::Capturer` over `screenshot_cpu`.
5. **Streaming composite** (Vulkan on Linux).
6. **Windows** (WGC, DXGI, GDI, D3D11 composite), **macOS** (SCK).
7. **Android** (MediaProjection, preview target, grant handle).
8. **iOS 27 SCK** (picker, preview layer); ReplayKit legacy.
9. **Web**.

A backend joins `Auto` only once its checklist passes; no step leaves a
platform half-wired.

## Rejected alternatives

- **Encoded screenshots (PNG) in the plugin**: encoding is the app's
  choice and dependency; raw pixels serve every consumer.
- **Plugin-side restore-token storage**: the app knows the user's
  intent ("remember my choice") and owns its secret store; the plugin
  would need a secret-store dependency per platform.
- **Consent gating of `Auto` / an `allow_silent_capture` switch**:
  breaks ferricast's preview path and duplicates a decision the app
  already makes; honest reporting is enough.
- **One `open` with an optional audio sink**: `Option<FrameSinkId>` is
  not a legal contract shape, and preview has no sink.
- **Timeline sync in frames**: no portal producer ships it and DMA-BUF
  backings carry no syncobj; the producer waits and says `Ready`.
- **Streaming limited to one monitor**: spanning regions are required;
  the API carries them from day one.
- **A generic `Known<T>` wrapper for provenance**: generic message types
  are not legal; per-section `CapsProvenance` instead.
- **Silent CPU fallbacks** (ferricast's wl_shm switch): replaced by
  listed tiers + `Downgraded` and the `Exhaustion` policy.
- **iOS broadcast-extension ring as the system-wide path**: iOS 27 SCK
  makes it unnecessary; before 27 it only works while the app is alive.
- **Picker fallback when a window id cannot be honoured**: an error
  unless `allow_picker`.
- **Desktop preview surfaces**: Rust desktop consumers already render
  from DMA-BUF / D3D11 frames; a surface mode adds platform UI code for
  no consumer.
