# `istmo-screen-capture`: platform reference

Companion to `design/screen-capture.md`. Per platform: which OS
version brings which feature, what can be known before consent, what
each backend must get right, and where the facts come from. The main
design refers to rows here by backend id.

Legend for the "Known before consent" columns, used by the capability
report's `Provenance`:

- **Probe**: asked from the running system without any prompt.
- **Table**: from this document, keyed by OS version / protocol
  version; correct for the platform but not checked on this machine.
- **Negotiated**: only known once `open` returned a `NegotiatedStream`
  (needs consent, a picker choice or a first frame).

## Linux

### Version table

| Feature | Requires | Source |
|---|---|---|
| ScreenCast portal `cursor_mode`, `AvailableCursorModes` | portal interface v2 | [ScreenCast portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html) |
| `source_type` stream property | v3 | same |
| `restore_token`, `persist_mode`, stream `id` | v4 | same |
| `mapping_id` stream property | v5 | same |
| `pipewire-serial` stream property | v6 | same |
| `ext-image-copy-capture-v1` | compositor: Sway 1.11, Hyprland, COSMIC, labwc, river, Jay. **Not** Mutter, KWin, niri 26.04, Weston, Wayfire | [wayland.app ext-image-copy-capture-v1](https://wayland.app/protocols/ext-image-copy-capture-v1) (compositor support table) |
| `zwlr_screencopy_manager_v1` | wlroots compositors, niri, Hyprland; deprecated upstream in favour of ext-image-copy | [wayland.app wlr-screencopy-unstable-v1](https://wayland.app/protocols/wlr-screencopy-unstable-v1) |
| PipeWire `SPA_META_SyncTimeline` producers | none merged: Mutter !3682 is a draft, KWin !6204 open | Mutter / KWin merge requests |
| X11 DRI3 `BuffersFromPixmap` (multi-plane) | DRI3 ≥ 1.2 | [dri3proto](https://gitlab.freedesktop.org/xorg/proto/xorgproto/-/blob/master/dri3proto.txt) |

Restore tokens are **single-use**: "each new session must use the token
from the previous one"; `persist_mode` is `0` do not persist, `1` while
the application runs, `2` until revoked (ScreenCast portal doc).

### Known before consent

| Fact | Portal | ext-image-copy | wlr-screencopy | X11 | KMS |
|---|---|---|---|---|---|
| Source kinds | Probe (`AvailableSourceTypes`) | Probe (capture source managers bound) | Probe (outputs only) | Probe | Probe |
| Cursor modes | Probe (`AvailableCursorModes`, v2) | Probe (cursor session manager) | Table (overlay_cursor flag) | Probe (XFixes) | Table (none) |
| Pixel formats / modifiers | Negotiated (PipeWire `EnumFormat` after `Start`) | Negotiated (session `shm_format` / `dmabuf_format` events, per source) | Negotiated (per-frame `buffer` / `linux_dmabuf` events) + Probe (`linux-dmabuf` feedback) | Probe (visual depth, DRI3 version) | Probe (`drmModeGetFB2`) |
| Producer GPU | Negotiated | Negotiated (`dmabuf_device`) | Probe (feedback main device) | Probe (DRI3 open) | Probe |
| Damage | Table (`SPA_META_VideoDamage` optional) | Table (per frame, protocol) | Table (`copy_with_damage`) | Probe (XDamage) | Table (none) |
| Explicit sync | Negotiated (never assumed) | Table (none in protocol) | Table (none; `ready`) | Table (implicit) | Table (none) |
| Persistence | Probe (portal version ≥ 4) | Table (none needed) | Table (none needed) | Table | Table |
| Source list | not enumerable (picker) | Probe (`ext_foreign_toplevel_list_v1` + outputs) | Probe (outputs) | Probe | Probe |

### Backend notes

**Portal + PipeWire** (`linux-portal`, port of ferricast
`capture/src/pipewire`):

- `CreateSession` → `SelectSources` (`types`, `multiple`, `cursor_mode`
  when v ≥ 2, `persist_mode` + `restore_token` when v ≥ 4) → `Start`.
  The token returned by `Start` replaces the one passed in (single
  use); the plugin returns it to the app, never stores it.
- PipeWire connection is a session option
  (`LinuxOptions::pipewire`): `PortalRemote` (default:
  `OpenPipeWireRemote` fd, works in Flatpak) or `SessionDaemon`
  (`Context::connect(None)`, the user's daemon; ferricast's choice
  because it avoids per-portal quirks of the private fd, see its
  `pipewire/stream.rs` header). Both use `DONT_RECONNECT`, and a stream
  that falls back to `Unconnected` after `Streaming` is fatal
  (`SourceLost`), as ferricast's
  `unconnected_after_connect_and_errors_are_fatal` pins.
- Format negotiation per format tier (see main design, "Format
  tiers"): one `EnumFormat` pod per tier with `MANDATORY | DONT_FIXATE`
  modifiers in the tier's order, then the next tier, then SHM if a CPU
  tier was listed. `param_changed` always answers `[Buffers, Meta]`.
- `SPA_META_SyncTimeline`: offered as the first `ParamBuffers` variant
  only. A producer that adds it gets its acquire point waited on
  (bounded, 100 ms, on the loop thread) and frames leave as
  `FrameSync::Ready`; the release point is signalled from the release
  hook, posted to the loop thread. Never required, never exposed to
  consumers (no Timeline sync in the data plane).
- Without the meta: `FrameSync::Implicit`, plus the
  `nvidia-implicit-sync-best-effort` note when the producer GPU is
  NVIDIA (NVIDIA attaches no implicit fences).
- Metas requested: `VideoDamage`, `Cursor`, `VideoTransform`,
  `VideoCrop`, `Header`.
- Buffers are held, not re-queued, until the consumer drops the frame;
  the hold limit is "all but one" (ferricast `lease.rs`,
  `holds_all_but_one_buffer`). Requeue always runs on the loop thread:
  the release hook posts to it.
- `process` drains stale buffers and keeps only the newest
  (ferricast).
- Multi-monitor: `multiple = true` yields one PipeWire stream per
  chosen monitor; the composite path (main design) joins them.

**ext-image-copy-capture-v1** (`linux-ext-image-copy`, port of sss
`ext_image_copy`):

- Sources from `ext_output_image_capture_source_manager_v1` and
  `ext_foreign_toplevel_image_capture_source_manager_v1` (windows via
  `ext_foreign_toplevel_list_v1`).
- Session constraints (`buffer_size`, `shm_format`, `dmabuf_device`,
  `dmabuf_format` + modifiers) drive allocation; a constraints change
  bumps `pool_generation` and reallocates.
- Damage per frame; `presentation_time` → `pts_ns`; cursor sessions
  for `CursorMode::Metadata`.
- No sync in the protocol: frames leave as `Ready` after `ready`
  (the compositor has finished the copy), with the same NVIDIA caveat
  as wlr (`ready` is safe only where the compositor waits on its render
  before sending it).

**wlr-screencopy** (`linux-wlr-screencopy`, port of ferricast
`wayland_direct`):

- DMA-BUF allocated through GBM on the `linux-dmabuf` feedback main
  device. Modifier list = feedback tranches for the fourcc ∩ the
  caller's tier list, tier by tier; `INVALID` (implicit) is never
  chosen; NVIDIA refuses LINEAR + RENDERING so LINEAR is never assumed
  (ferricast `layout_*` tests).
- `copy_with_damage` by default; full `copy` on request.
- No transform event in the protocol: orientation comes from the
  output's `wl_output.geometry` transform composed with the frame's
  `Y_INVERT` flag.
- `ready` = copy complete. Frames leave as `Ready`; correct on niri
  (it sends `ready` only after its render's sync point signalled,
  ferricast NVCODEC.md "Capture zero-copy"). On compositors not known
  to wait, frames leave as `Implicit` with the NVIDIA note.
- Pool exhaustion: today ferricast silently switches to `wl_shm`; here
  that becomes an explicit lower tier (`Downgraded` event) or the
  `Exhaustion` policy, never silent.

**X11** (`linux-x11`, port of sss `x11`):

- Detection: X11 only when there is no Wayland session
  (`WAYLAND_DISPLAY` unset and `XDG_SESSION_TYPE != wayland`). A
  `DISPLAY` inside a Wayland session is XWayland and must not select
  X11 (it would capture only X clients, black elsewhere).
- XShm `GetImage` for monitors, Composite `NameWindowPixmap` for
  windows (occluded content works; unmapped / minimised windows give no
  frames → `SourceHidden`), XDamage for damage and `OnDamage` rate,
  XFixes cursor image.
- DRI3 `BuffersFromPixmap` (≥ 1.2) gives a DMA-BUF of the **live**
  pixmap: the content changes under the consumer. Offered only with
  the `live-buffer` note and never in a zero-copy tier unless the caller
  allowed `LiveBuffers`; otherwise the plugin copies into its own pool
  (declared `CopyStage::LiveBufferSnapshot`).
- No per-monitor scale (`scale` = 1.0, `Provenance::Table`).

**KMS** (`linux-kms`, opt-in twice: cargo feature **and**
`CapturePolicy::allow_kms`):

- Needs `CAP_SYS_ADMIN` / DRM master; `drmModeGetFB2` + PRIME export
  (`DRM_CLOEXEC`) of the primary plane only: misses overlays and the
  cursor plane, vendor modifiers, no sync, live buffer. Reported with
  the `kms-bypasses-compositor` note (it ignores every compositor-side
  protection, which is why it is double opt-in).

## Windows

### Version table

| Feature | Requires | Source |
|---|---|---|
| `GraphicsCaptureSession` | Windows 10 1803 (17134) | [GraphicsCaptureSession](https://learn.microsoft.com/uwp/api/windows.graphics.capture.graphicscapturesession) |
| `IGraphicsCaptureItemInterop` (HWND / HMONITOR) | 1903 (18362) | [IGraphicsCaptureItemInterop](https://learn.microsoft.com/windows/win32/api/windows.graphics.capture.interop/nn-windows-graphics-capture-interop-igraphicscaptureiteminterop) |
| `IsCursorCaptureEnabled` | 2004 (19041) | GraphicsCaptureSession version history |
| `IsBorderRequired` | build 20348 | [IsBorderRequired](https://learn.microsoft.com/uwp/api/windows.graphics.capture.graphicscapturesession.isborderrequired) |
| `GraphicsCaptureAccess.RequestAccessAsync(Borderless)` | build 20348 + `graphicsCaptureWithoutBorder` capability (packaged apps) | same page, remarks |
| `MinUpdateInterval`, `DirtyRegionMode`, frame `DirtyRegions` | UniversalApiContract v19 (Windows 11 24H2, 26100) | [DirtyRegionMode](https://learn.microsoft.com/uwp/api/windows.graphics.capture.graphicscapturedirtyregionmode) |
| Process loopback audio (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`) | build 20348 | [Application loopback sample](https://learn.microsoft.com/samples/microsoft/windows-classic-samples/applicationloopbackaudio-sample/) |
| Desktop Duplication | Windows 8; `DuplicateOutput1` (HDR / FP16) Windows 10 1703 | [IDXGIOutput1::DuplicateOutput](https://learn.microsoft.com/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutput1-duplicateoutput), [IDXGIOutput5::DuplicateOutput1](https://learn.microsoft.com/windows/win32/api/dxgi1_5/nf-dxgi1_5-idxgioutput5-duplicateoutput1) |

Borderless denial is silent: "setting this property to false will
succeed, but the value will be ignored and the border will be
displayed" (IsBorderRequired remarks). The plugin reports the border
state from the consent result, not from the property.

More rows the backends rely on:

| Feature | Requires | Source |
|---|---|---|
| `Direct3D11CaptureFramePool.CreateFreeThreaded` | Windows 10 1809 (17763) | [Direct3D11CaptureFramePool](https://learn.microsoft.com/uwp/api/windows.graphics.capture.direct3d11captureframepool) version history |
| `IncludeSecondaryWindows`, `MinUpdateInterval`, `DirtyRegionMode` | no build stated: probed with `ApiInformation.IsPropertyPresent` | [GraphicsCaptureSession](https://learn.microsoft.com/uwp/api/windows.graphics.capture.graphicscapturesession) |
| `IDXGIResource1::CreateSharedHandle` | Windows 8; the resource created `SHARED_NTHANDLE` (with `SHARED` or `SHARED_KEYEDMUTEX`) | [CreateSharedHandle](https://learn.microsoft.com/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgiresource1-createsharedhandle) |
| `DuplicateOutput1` errors: `E_ACCESSDENIED` (secure desktop), `DXGI_ERROR_UNSUPPORTED` (mode), `NOT_CURRENTLY_AVAILABLE` (4 duplicators), `E_INVALIDARG` (already duplicated by this process) | — | [DuplicateOutput1](https://learn.microsoft.com/windows/win32/api/dxgi1_5/nf-dxgi1_5-idxgioutput5-duplicateoutput1) |
| `ACCESS_LOST` on desktop switch / mode change / DWM change: re-create the duplication | — | [AcquireNextFrame](https://learn.microsoft.com/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-acquirenextframe) |
| `AUDIOCLIENT_ACTIVATION_PARAMS`, `PROCESS_LOOPBACK_MODE_{INCLUDE,EXCLUDE}_TARGET_PROCESS_TREE` | build 20348 | [AUDIOCLIENT_ACTIVATION_PARAMS](https://learn.microsoft.com/windows/win32/api/audioclientactivationparams/ns-audioclientactivationparams-audioclient_activation_params) |

### Known before consent (Windows)

No Windows backend asks for consent, so "Negotiated" only means "after
`StartCapture` / `DuplicateOutput`".

| Fact | WGC | DXGI | GDI |
|---|---|---|---|
| Source kinds | Table (monitors, windows) | Table (monitors) | Table (monitors, windows) |
| Source list | Probe (`EnumDisplayMonitors`, `EnumWindows`) | Probe | Probe |
| Pixel formats | Table (BGRA8, FP16) | Probe (`IDXGIOutput5` → FP16 / 10-bit possible); the format itself Negotiated | Table (BGRX8) |
| Cursor modes | Probe (`IsCursorCaptureEnabled`) | Table (hidden, metadata) | Table |
| Damage | Probe (`DirtyRegionMode`) | Table (dirty / move rects) | Table (none) |
| Producer GPU | Probe (first hardware adapter, or the preferred one) | Probe (the output's adapter) | Table (none: CPU) |
| Border / persistence | Probe (`IsBorderRequired`, the capability status) | Table (none) | Table (none) |

### Backend notes

**WGC** (`windows-wgc`): item from `CreateForMonitor` /
`CreateForWindow`; free-threaded frame pool on the D3D11 device the
caller prefers (adapter LUID). Formats `B8G8R8A8UIntNormalized` and
`R16G16B16A16Float` (scRGB HDR) only; NV12 needs a GPU conversion
(`CopyStage::FormatConversion`). Cursor: embedded or hidden only (no
metadata). Minimised windows deliver no frames (`SourceHidden`);
`WDA_EXCLUDEFROMCAPTURE` and DRM content come out black (reported as
`ProtectedContent::MaskedUndetectable`). Capture textures belong to the
pool's device and are not shareable: a consumer on another device gets
a copy into a `D3D11_RESOURCE_MISC_SHARED_NTHANDLE` texture
(`CopyStage::CrossDevice`, declared in caps and in `NegotiatedStream`).
`SystemRelativeTime` is QPC.

**DXGI Desktop Duplication** (`windows-dxgi`):

- One duplication per output per process; the device must be on the
  adapter driving the output, so a `GpuPreference` for another adapter
  cannot be honoured (reported, `CopyStage::CrossDevice` if accepted).
- Only **one** acquired frame at a time (`AcquireNextFrame` /
  `ReleaseFrame`): the plugin must `CopyResource` into its own pool
  before releasing. This copy is mandatory and declared
  (`CopyStage::DxgiMandatory`), so DXGI is never in a "no copy" tier.
- Rotated outputs are delivered un-rotated: `DXGI_OUTDUPL_DESC.Rotation`
  → `FrameMeta.orientation`.
- Dirty / move rects and pointer shape (`CursorMode::Metadata`
  natively); protected content masked; secure desktop (UAC, lock) →
  `E_ACCESSDENIED` → `Paused(SecureDesktop)`; `DXGI_ERROR_ACCESS_LOST`
  → re-create, `pool_generation` bump, `Renegotiated`.

**GDI** (`windows-gdi`): `BitBlt` / `PrintWindow(PW_RENDERFULLCONTENT)`,
CPU. Screenshots, and slow streams polled at the session rate with no
damage.

## macOS

### Version table

| Feature | Requires | Source |
|---|---|---|
| `SCStream`, `SCShareableContent`, `SCContentFilter`, `SCStreamFrameInfo` | macOS 12.3 | [SCStream](https://developer.apple.com/documentation/screencapturekit/scstream), [SCShareableContent](https://developer.apple.com/documentation/screencapturekit/scshareablecontent) |
| `SCScreenshotManager` | 14.0 | [SCScreenshotManager](https://developer.apple.com/documentation/screencapturekit/scscreenshotmanager) |
| `SCContentSharingPicker` | 14.0 | [SCContentSharingPicker](https://developer.apple.com/documentation/screencapturekit/sccontentsharingpicker) |
| `presenterOverlayPrivacyAlertSetting` | 14.0 | [presenterOverlayPrivacyAlertSetting](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/presenteroverlayprivacyalertsetting) |
| `includeChildWindows` | 14.2 | [includeChildWindows](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/includechildwindows) |
| `persistent-content-capture` entitlement (VNC-type apps, Apple approval) | 14.4 | [entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.persistent-content-capture) |
| `captureDynamicRange` (HDR), `captureMicrophone`, configuration presets | 15.0 | [captureDynamicRange](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/capturedynamicrange), [captureMicrophone](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/capturemicrophone) |
| Multi-display screenshots | per "ScreenCaptureKit updates" | [updates](https://developer.apple.com/documentation/updates/screencapturekit) |
| `SCStreamFrameInfo.videoOrientation` | 27.0 | [videoOrientation](https://developer.apple.com/documentation/screencapturekit/scstreamframeinfo/videoorientation) |
| `CGRequestScreenCaptureAccess` / `CGPreflightScreenCaptureAccess` | 10.15 | [CGRequestScreenCaptureAccess](https://developer.apple.com/documentation/coregraphics/cgrequestscreencaptureaccess()) |
| `CGWindowListCreateImage` | deprecated 14, unavailable in the macOS 15 SDK | [CGWindowListCreateImage](https://developer.apple.com/documentation/coregraphics/cgwindowlistcreateimage(_:_:_:_:)) |
| ReplayKit `RPScreenRecorder` | deprecated 27.0 | [RPScreenRecorder](https://developer.apple.com/documentation/replaykit/rpscreenrecorder) |

Pixel formats: `BGRA`, `l10r` (ARGB2101010), `420v`, `420f`
([pixelFormat](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/pixelformat)).
`queueDepth`: default and minimum 3, "don't exceed" 8
([queueDepth](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/queuedepth));
the plugin clamps `pool_depth` into 3..=8 and reports the result.

### Known before consent

`SCShareableContent` needs the TCC grant: before it, displays /
windows / apps are **not** enumerable (`SourceListing::AfterGrant`).
Formats, queue depth, cursor (embedded / hidden, no metadata) and
colour come from the table. Producer GPU: Negotiated (IOSurface's
registry id on the first frame).

### Backend notes

**SCK** (`macos-sck`): `SCContentFilter` (display minus excluded apps /
windows, one window, one app); `SCStreamConfiguration` (`pixelFormat`,
`colorSpaceName`, `showsCursor`, `minimumFrameInterval`, `queueDepth`,
`capturesAudio`, `excludesCurrentProcessAudio`); frame attachments
(`SCFrameStatus.idle` → `Damage::Unchanged`, `dirtyRects`,
`contentRect`, `scaleFactor`). Backing: retained `IOSurface` /
`CVPixelBuffer` (no copy). Stop: `SCStreamDelegate
didStopWithError` → `ConsentRevoked` or `SourceLost` by error code.

TCC: grant tied to the code signature (re-signing loses it);
`CGRequestScreenCaptureAccess` prompts at most once and the process
must relaunch before the grant applies (`AccessStatus::RequiresRestart`);
macOS 15 re-prompts monthly for apps without the
`persistent-content-capture` entitlement, and the picker does not
reliably avoid that prompt.

Screenshots: `SCScreenshotManager` on 14+; on 12.3–13 a one-frame
`SCStream`. `CGWindowList` is not used (unavailable in the 15 SDK the
plugin builds with).

## iOS / iPadOS

### Version table

| Path | Requires | Source |
|---|---|---|
| ScreenCaptureKit in-process (`SCStream`, `SCContentSharingPicker`, `SCContentFilter`) | iOS / iPadOS 27 | [Capturing screen content on iOS](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-on-ios), [SCStream](https://developer.apple.com/documentation/screencapturekit/scstream) |
| Background capture | `UIBackgroundModes` `screen-capture` (+ `audio` for the mic tap) | same article |
| `captureDynamicRange` | iOS 27 | [captureDynamicRange](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/capturedynamicrange) |
| `SCShareableContent` | **not on iOS** (macOS / Catalyst only) → picker only | [SCShareableContent](https://developer.apple.com/documentation/screencapturekit/scshareablecontent) |
| ReplayKit in-app `RPScreenRecorder.startCapture` (legacy) | iOS 11, deprecated 27 | [RPScreenRecorder](https://developer.apple.com/documentation/replaykit/rpscreenrecorder) |
| Broadcast picker / upload extension (legacy) | iOS 12, deprecated 27 | [RPSystemBroadcastPickerView](https://developer.apple.com/documentation/replaykit/rpsystembroadcastpickerview) |

The iOS 27 sample presents the shared `SCContentSharingPicker` for a
full-display or an in-app capture, receives the chosen
`SCContentFilter` through the picker observer and starts an `SCStream`
from it; full-display capture keeps running in the background with the
`screen-capture` background mode.

### Backend notes

**SCK on iOS 27** (`ios-sck`, primary): Swift backend; picker-only
(`SourceListing::PickerOnly`, `allow_picker` must be true for any
capture); in-app capture (only the app's own windows / layers) or full
display, as the person picks; `SCStreamFrameInfo.videoOrientation` →
`FrameMeta.orientation`; `CVPixelBuffer` handed to the sink without a
copy (`istmo_ios_submit_native_buffer`). Sample buffers can be enqueued
at the same time into an `AVSampleBufferDisplayLayer` for the preview
target (retain, no copy).

**ReplayKit in-app** (`ios-replaykit`, legacy < 27): foreground only
(stops in background → `Paused(Backgrounded)`), system alert on every
`startCapture`, `RPVideoSampleOrientationKey` → orientation,
timestamps are host time (`mach_absolute_time`, does not advance in
sleep) → `pts_clock = HostTime`; buffers come from a system pool, so
holding them stalls capture (pool depth is the system's; `Exhaustion`
applies).

**Broadcast upload extension** (legacy < 27, transport deferred): not
part of the first iOS step. Constraints recorded for when it is built:
separate process, 50 MB memory ceiling, no Rust runtime in it, no
`IOSurface` across to the app, and an extension→app shared-memory ring
only works while the app is alive (a suspended app reads nothing), so
an extension that must work with the app in the background has to
encode and send by itself. Hardening of the ring, if built, is in the
main design ("Security").

## Android

### Version table

| Feature | Requires | Source |
|---|---|---|
| MediaProjection | API 21 | [Media projection](https://developer.android.com/media/grow/media-projection) |
| `ImageReader.getHardwareBuffer` (`Image.getHardwareBuffer`) | API 28 | [Image.getHardwareBuffer](https://developer.android.com/reference/android/media/Image#getHardwareBuffer()) |
| `ImageReader.newInstance(w, h, format, maxImages, usage)` | API 29 | [ImageReader.newInstance](https://developer.android.com/reference/android/media/ImageReader#newInstance(int,%20int,%20int,%20int,%20long)) |
| `AudioPlaybackCaptureConfiguration` | API 29 (`USAGE_MEDIA` / `GAME` / `UNKNOWN`; apps targeting ≤ 28 not capturable; apps may opt out) | [Capture audio playback](https://developer.android.com/media/platform/av-capture) |
| `Image.getFence` | API 33 | [Image.getFence](https://developer.android.com/reference/android/media/Image#getFence()) |
| `POST_NOTIFICATIONS` runtime permission (FGS notification) | API 33 | [Notification permission](https://developer.android.com/develop/ui/views/notifications/notification-permission) |
| `mediaProjection` FGS type + permission; single-use consent; app screen sharing; `onCapturedContentResize` | API 34 | [Media projection](https://developer.android.com/media/grow/media-projection) |
| Status bar chip, projection stops on screen lock (`onStop`) | Android 15 QPR1 | same page, "Status bar chip and auto stop" |
| Uniform letterboxing into the surface | API 32 (12L) | same page, "Surface" |

From the official page: "A session is a single call to
`createVirtualDisplay()`. A `MediaProjection` token must be used only
once", and on 14+ a second `getMediaProjection` with the same intent or
a second `createVirtualDisplay` on the same projection throws
`SecurityException`; without a registered callback
`createVirtualDisplay` throws `IllegalStateException`; manufacturers
can override an app's opt-out of app screen sharing.

### Backend notes

**MediaProjection** (`android-projection`, Kotlin backend):

- Consent intent through the activity-results plumbing; on API 34+ the
  `IstmoProjectionService` (`foregroundServiceType="mediaProjection"`,
  `exported="false"`) is in the foreground before `getMediaProjection`;
  `MediaProjection.Callback` registered before `createVirtualDisplay`.
- Frames: `ImageReader` with `PRIVATE` + `GPU_SAMPLED_IMAGE`
  (or `RGBA_8888` + `CPU_READ_OFTEN` for the CPU tier — the only
  supported combinations), `maxImages = pool_depth`,
  `HardwareBuffer.isSupported` probed. The `Image` stays open until
  Rust releases the frame (lease / ticket, see the mechanism list);
  `IllegalStateException` from `acquireNextImage` when all images are
  held = exhaustion. `Image.getFence` (33+) waited on before submit.
- Pixel formats: RGBA only (the virtual display composes RGBA); no YUV.
- Size: initial = `getMaximumWindowMetrics`; app sharing size is known
  only from `onCapturedContentResize` → `SourceResized` + VirtualDisplay
  / ImageReader resize (`pool_generation` bump); 12L+ letterboxing
  reported through `content_rect`.
- Timestamps: `Image.getTimestamp` is `CLOCK_MONOTONIC`
  (`pts_clock = Monotonic`), not `elapsedRealtime`.
- `FLAG_SECURE` / `setScreenCaptureDisabled` content is black and
  undetectable (`ProtectedContent::MaskedUndetectable`).
- Audio: `AudioRecord` + `AudioPlaybackCaptureConfiguration`
  (`RECORD_AUDIO` needed); silence possible (opted-out apps) →
  `AudioCaps.may_be_silent = true`.
- Preview target: the VirtualDisplay renders straight into the app's
  `Surface` (SurfaceView / TextureView / `MediaCodec` input surface).

## Web

| Feature | Notes | Source |
|---|---|---|
| `getDisplayMedia` | transient user activation needed on every call; no persisted permission | [MDN getDisplayMedia](https://developer.mozilla.org/docs/Web/API/MediaDevices/getDisplayMedia) |
| `displaySurface`, `selfBrowserSurface`, `surfaceSwitching`, `systemAudio`, `preferCurrentTab` | hints, mostly Chromium; `getSupportedConstraints()` is the only probe | [MDN MediaTrackConstraints](https://developer.mozilla.org/docs/Web/API/MediaTrackConstraints) |
| `cursor` constraint | support unverified → cursor `Embedded`, `Provenance::Table` | same |
| `MediaStreamTrackProcessor` | Chromium; worker on some versions | [MDN MediaStreamTrackProcessor](https://developer.mozilla.org/docs/Web/API/MediaStreamTrackProcessor) |
| `VideoFrame.copyTo` | always a copy into wasm memory | [MDN VideoFrame.copyTo](https://developer.mozilla.org/docs/Web/API/VideoFrame/copyTo) |

A web screenshot is one picker per call unless the app keeps a session
open. `VideoFrame` is `!Send`; its backing needs the wasm32
single-thread allowance listed in the main design. Preview target: an
`HTMLVideoElement` given the `MediaStream` (`srcObject`), no copy.
