# Changelog

## Unreleased

### Added

- `istmo.toml` `[plugin] targets = ["android", "ios", "desktop", "web"]`:
  an app generates a plugin's Kotlin / Swift sources, registry entries
  and shared type groups only for the mobile targets it lists. Default:
  every target.
- `istmo-menu`: menu model shared by menu plugins (shared type group
  `IstmoMenu`). Flat node list with parent links, check / radio / header
  / separator / submenu items, shortcuts, subtitles, badges, icons
  (SVG / PNG / JPEG / platform names, inline or registered), patches,
  validation, automatic check toggling and radio exclusivity, and a diff
  that turns a replacement menu into property patches when nothing moved.
- `istmo-tray` plugin (`istmo.tray`, desktop and web targets): tray items
  with an `istmo-menu` menu, static or animated SVG / PNG / JPEG icons
  (decoded once with `register_icon`), title, tooltip, visibility and an
  event stream (clicks, scroll, menu open / close, item activation,
  check / radio changes, tray host presence). Changes coalesce per item.
  Linux backend: `StatusNotifierItem` + `com.canonical.dbusmenu` over
  zbus, one bus connection per item, re-registration when the watcher
  comes back. Windows backend: `Shell_NotifyIconW` (version 4) on a hidden
  top-level window, popup menus built when they open (radio items
  native, mixed checks drawn as a dash, default item, shortcut text),
  icons added again on `TaskbarCreated`, optional `windows_guid`.
  macOS backend: `NSStatusItem` + `NSMenu` driven from the main queue
  (callers never wait for the main thread), radio items with a dot
  image, section headers / subtitles / badges where macOS 14 has them,
  template images, SF Symbol names, and `run_event_loop` for tray-only
  apps.
  Web backend (`WebTray`): the favicon, one item per document, images
  passed to the browser as Blob URLs, animations on `setTimeout`.
  `UnavailableTray` for Android / iOS.
- `istmo-screen-capture` plugin (`istmo.screen_capture`): screenshots
  (frames into a sink, or raw pixels with `screenshot_pixels`, never
  encoded; multi-monitor CPU composite) and live sessions into frame
  sinks, with a capability report that says per section whether a fact
  was probed, comes from a table or is known only after consent.
  Linux backends behind features: `linux-wlr-screencopy` (GBM DMA-BUF
  zero-copy), `linux-ext-image-copy`, `linux-x11`, `linux-portal`
  (PipeWire, shared memory only for now) and `linux-audio` (PipeWire).
  Android and iOS backends answer `NoBackendCompiled` for now.
  `scripts/check-plugin-native.sh` compiles a plugin's `native/`
  backends against its generated Kotlin / Swift.
- Screen capture `Source`: `make`, `model`, `connector` and
  `description` of monitors (Wayland `wl_output` / xdg-output, X11
  `RandR` output names and `EDID`).
- Screen capture lists Wayland windows from the compositor's toplevel
  list (`ext_foreign_toplevel_list_v1`, else
  `zwlr_foreign_toplevel_manager_v1`), also on backends that capture
  outputs only (wlr-screencopy, niri). `Source::capture`
  (`SourceCapture::ById` / `PickerOnly(backends)`) says whether an id
  captures it or only a picker of those backends can.
- Screen capture `watch_sources` (stream, same-process only): the source
  listing, then `Added` / `Changed` / `Removed` as monitors are plugged
  or reconfigured (Wayland `wl_output` / xdg-output, X11 `RandR`) and
  windows open, close, retitle or refocus. Rust sugar
  `ScreenCaptureClientExt::source_watch` keeps the current list
  (`SourceWatch`). Android and iOS answer `NoBackendCompiled`.
- Screen capture audio-only sessions: `open_audio` (sink-taking,
  same-process only) with the rate, channels and format asked per open,
  its own handle (`AudioCaptureSession`, `AudioSessionTag`) and
  `update_audio` (`Pause` / `Resume` / `Mute` / `Unmute`),
  `close_audio`, `audio_events` (`AudioEvent`). Chunks keep the
  sample-count UNIX-epoch `wall_us`. `CapabilityReport.audio`
  (`AudioReport`) says whether audio capture runs here. Rust sugar
  `open_audio_session` / `AudioSessionHandle`. Android and iOS answer
  `NoBackendCompiled`.
- `istmo-media` crate: message types that mean the same in several media
  plugins (for now `AccessStatus`, used by camera and screen capture).
  Each shared type is generated once per app (Kotlin package
  `dev.istmo.media`, Swift `Plugins/IstmoMedia/Generated/IstmoMediaTypes.swift`)
  however many plugins use it. A shared crate calls
  `istmo_build::emit_shared_types` from its build script; plugins that
  disagree on a shared type's shape fail the app build.
- `NativeBuffer::into_dmabuf`: moves the `Arc<DmaBuf>` out together with
  the `BufferLease` that holds the release hooks.
- Native frame sinks (`istmo_core::native_frame`): `FrameSink` /
  `FrameSender` / `NativeBuffer` move buffers (DMA-BUF,
  `AHardwareBuffer`, `CVPixelBuffer` / `IOSurface` / `CMSampleBuffer`,
  CPU memory) from a producer to a consumer without encoding or copying.
  `FrameSink` is also a `futures_core::Stream`; `end_reason()` tells why
  a sink ended; `FrameSender::close_with(error)`.
- `HostCtx` host-method argument: the call's origin and runtime
  (`ctx.frame_sender(id)`).
- `#[istmo::blocking]` method attribute, `#[plugin(shortcut = false)]`,
  `#[plugin(native = false)]`, `runtime!(hosts: [#[wire_only] T => …])`,
  `Runtime::register_host_wire_only`, `RuntimeInit::host_wire_only`.
- Generated clients: `is_local()`. `IstmoError::domain_error::<E>()`
  decodes a plugin's domain error.
- Kotlin `IstmoFrameSender` / Swift `IstmoFrameSender` with
  `IstmoSubmitStatus.shouldStop`; Kotlin `IstmoRuntime.registerObject` /
  Swift `IstmoRuntime.registerObject(_:)` hand app objects (a preview
  surface) to plugins as `NativeHandleId`s.
- Native entry version check (`nativeAbiVersion` / `istmo_abi_version`):
  the Kotlin and Swift runtimes refuse a mismatched Rust library at start.
- Direct paths between Rust and the platform:
  `Dispatch::method_mode`; `Runtime::serve_peer_call` (an inline host's
  answer comes back from the call itself), `serve_peer_stream` +
  `grant_stream_credit` (credit-based peer streams);
  `DirectPeer` / `DirectOutcome` + `Runtime::install_direct_peer`,
  `outbound_delivered`, `track_outbound_deliveries` (Rust → Kotlin calls
  and stream items on the caller's thread); `DeliveryScope` (a thread
  that must never block on the outbound channel).
- JNI `nativeCall`, `nativeSubmitStream`, `nativeStreamCredit`; Kotlin
  callbacks `onCallDirect` and `onEvents`; C `istmo_ios_call` +
  `IstmoIosReply` + `istmo_ios_free_reply`. `nativeSubmitEvent` and
  `nativeSubmitStreamEnd` return `false` for an item or end that a
  thread that must not block could not deliver (Kotlin resubmits it
  from `Dispatchers.IO`).
- `istmo_core::spin::SpinWait` (bounded adaptive spinning before a park,
  `spin::set_enabled` for A/B); `native_frame::BytePool` (recycled CPU
  frame buffers); Kotlin `IstmoBytesOut`.
- `istmo.toml` `android_start_in_place = false`: the generated registry
  registers that plugin's backend with `startInPlace = false`.
- `Runtime::outbound_spill()` (`SpillStats`: frames queued behind a full
  outbound channel), `BlockingThreads::now()` (blocking workers alive
  and their peak; a warning past 256), `DeliveryScope::may_block()`,
  `Runtime::offer_peer_event`, `Runtime::offer_peer_stream_end`.
- Screen capture Windows backends (default features, inert elsewhere),
  not yet in `Auto` (named with `Fixed` / `Ordered` until their
  checklist passes on a real Windows session):
  - `windows-wgc`: Windows.Graphics.Capture, monitors and windows
    (Windows 10 1903+). Zero-copy frame-pool textures, shareable
    NT-handle copies for another device (`GpuPreference::Same` +
    `CopyStage::CrossDevice`), CPU readback, FP16 for HDR, dirty
    regions and `MinUpdateInterval` where the OS has them, cursor on /
    off, borderless capture behind `CapturePolicy::allow_borderless`.
  - `windows-dxgi`: Desktop Duplication of monitors
    (`CopyStage::DxgiMandatory` into an NT-handle pool), dirty / move
    rectangles, pointer metadata, scan-out rotation in
    `FrameMeta::orientation`, `protected_content`, `ACCESS_LOST`
    re-creation and `Paused(SecureDesktop)`.
  - `windows-gdi`: `BitBlt` / `PrintWindow` into pooled DIB sections.
  - `windows-audio`: WASAPI loopback of the default render endpoint,
    and process loopback (`ExcludeSelf`, `Application`) on build
    20348+, alone or with video, on the shared sample-count clock.
  - Monitor and window listing (EDID make / model, connector, refresh,
    executable as `app_id`), `watch_sources` (`WM_DISPLAYCHANGE` +
    WinEvent hooks), `Focused` / `UnderCursor`, and
    `access` / `request_access(BorderlessCapture)`.
  - `D3d11Frame`: the backing of `BufferMemory::D3d11Texture` frames
    (texture, device, adapter LUID, optional NT handle).
  - `examples/win_probe.rs`: the per-backend checklist on a Windows
    desktop, with a PASS / FAIL report.
- `CopyStage::GpuReadback`: a GPU texture read back into CPU memory.
  `FormatTier::cpu_rgb()` (and `FormatRequest::cpu()`, `cpu_frames()`,
  CPU screenshots) accepts it and `DxgiMandatory` next to
  `ShmTransport`.

### Changed (breaking)

- **iOS apps refer to plugin types as `<Plugin>.<Type>`.** Each plugin's
  generated Swift message types are nested in a `public enum <Plugin>`
  namespace (`Camera.SessionConfig`, `ScreenCapture.SessionConfig`,
  `Biometric.Availability`), types shared through `istmo-media` in
  `IstmoMedia` (`IstmoMedia.AccessStatus`), so plugins declaring the same
  type name can share one app. Generated codecs, clients, dispatchers and
  backend protocols use the qualified names; hand-written backends must
  too. Kotlin and the wire format are unchanged. Two namespaces, or a
  namespace and a generated symbol (`<T>Client`, `<T>Backend`, a runtime
  type), with the same name, and a message type that would shadow a name
  inside its namespace (`Data`, `Error`, `IstmoMedia`), fail the build
  (`istmo_build::check_swift_namespaces`). `TypeRef::to_swift` is
  removed; `generate_swift` names its protocols `<T>Interface` /
  `<T>InterfaceFactory`.
- **The typed in-process shortcut is installed by every host
  registration** (`runtime!(hosts: …)`, `RuntimeInit::host`,
  `Runtime::register_host`) for stateless plugins: a client in the same
  process polls the host's futures on its own task instead of a
  dispatcher thread. Methods that block or need a particular executor
  must be marked `#[istmo::blocking]`. `<Trait>Host::register` is gone.
- `#[plugin]` rejects argument and return types Kotlin and Swift cannot
  map, at the offending type (opt out with `native = false`).
- `TypedStream::into_inner` became `into_wire() -> Option<StreamHandle>`;
  a `TypedStream` whose item is `!Send` is `!Send` (local streams hold
  the producer's receiver).
- `Dispatch` gained `dispatch_from` (origin-aware) and `typed_shortcut`,
  both with defaults; `DispatchError::OriginRefused`.
- `IstmoError::FrameSinkTypeMismatch` names both metadata types;
  `IstmoError::FrameSinkNotShareable`.
- Generated Swift message types conform to `Sendable`; generated
  Kotlin / Swift clients have no methods taking frame sinks.
- The contract handover (`ISTMO_CONTRACTS`) starts with a format byte
  (`CONTRACTS_FORMAT`); a contract that does not decode fails the build.
  The format is now 3 (contracts carry the shared types they use): a
  format 2 handover is refused, so every plugin crate of an app must be
  built with the same `istmo-build` version.
- **Camera `AccessStatus` moved to `istmo-media`** and grew three states:
  - Rust: `istmo_camera::AccessStatus` is a re-export of
    `istmo_media::AccessStatus`, now `#[non_exhaustive]`, with
    `RequiresRestart`, `PerSession` and `NotApplicable` added. A `match`
    on it needs a wildcard arm.
  - Wire values 0..3 (`Granted`, `Denied`, `NotDetermined`, `Restricted`)
    are unchanged; the new states are 4..6.
  - Kotlin: `dev.istmo.camera.AccessStatus` is now
    `dev.istmo.media.AccessStatus` (update the imports), a `sealed class`
    (see `#[non_exhaustive]` below); an exhaustive `when` gains three
    branches and `is AccessStatus.Unknown`.
  - Swift: the type is `IstmoMedia.AccessStatus`, generated in
    `Plugins/IstmoMedia/Generated/IstmoMediaTypes.swift`, without a raw
    value; an exhaustive `switch` gains three cases and `.unknown`.
  - An app generated against an older `istmo-media` decodes a state added
    later as `Unknown` / `.unknown` instead of failing.
- **`#[non_exhaustive]` message enums keep variants they do not know.**
  Generated Kotlin and Swift give such an enum one more variant, Kotlin
  `Unknown(discriminant: Int, payload: ByteArray)` and Swift
  `.unknown(discriminant: UInt32, payload: Data)`, which an unknown
  discriminant decodes into (instead of a `DecodeException` /
  `DecodeError.invalidTag`) and which writes back the same bytes. The
  payload is the rest of the message, so a value read after it fails to
  decode: this helps where the enum is the last value of its message (a
  return value, an error, a stream item, a last field). Such an enum is a
  Kotlin `sealed class` (unit variants are `object`s; no `entries`,
  `ordinal` or `valueOf`) and a Swift enum without raw values (no
  `rawValue`; unit-only ones stay `Hashable`) even when every variant is a
  unit. Affects `AccessStatus`, `BarcodeFormat` and `CaptureError`: an
  exhaustive `when` / `switch` on them needs the new branch. Generated
  Rust types carry `#[non_exhaustive]` too. `EnumDef` gained
  `non_exhaustive`; a `#[non_exhaustive]` enum with a variant named
  `Unknown`, or carrying a type named `Unknown`, fails the build
  (`ContractError::ReservedUnknown`).
- `NativeBuffer::downcast_ref::<Arc<DmaBuf>>()` now returns `None`: a
  DMA-BUF backing is only borrowed (`dmabuf()`) or moved out with its
  lease (`into_dmabuf()`), so its fds cannot outlive the release hooks.
- **No thread per hosted call.** `DispatchMode::OnCallerExecutor` is
  removed (it was never implemented; `DispatchMode` is not
  `#[non_exhaustive]`, so a `match` on it breaks). `Inline` work starts
  on the thread that delivered it (the Rust caller, the Kotlin / Swift
  caller, the bridge reader) and continues on a few persistent async
  workers after its first wait; `Isolated` work runs on persistent
  blocking workers, an idle one or a new one (no cap, no queueing),
  `available_parallelism` of them kept, the others exiting after 30 s
  idle.
- **Generated hosts are `Inline`** on every path, stateful plugins
  (`init = …`) and calls from Kotlin, Swift and other processes
  included: a host method that blocks must be `#[istmo::blocking]` (the
  in-tree plugins are marked). A stateful plugin's in-process call runs
  its body inside `Runtime::call` on the caller's thread until it first
  waits. `register_host_wire_only` / `#[wire_only]` run every call of
  that host on a blocking worker; `#[plugin(shortcut = false)]` alone
  only removes the shortcut.
- **Notifications follow `method_mode`** (inline for generated hosts)
  instead of always getting a thread of their own.
- The desktop data-store serves reads inline (they never wait for a
  write); writes stay `#[istmo::blocking]`.
- A thread the outbound channel may depend on (a transport pump, a
  process bridge's reader, frame-sink ingress, a synchronous send on an
  async worker) never blocks on a full outbound channel: its frames
  queue behind the channel in order and a drain thread moves them on;
  cancels and releases queue the same way instead of being dropped.
  Other threads, callers polling an inline host included, keep the
  backpressure. `Runtime::outbound_spill()` counts the queue.
- `NATIVE_ABI_VERSION` 1 → 2 (`PROTOCOL_VERSION` stays 4): new JNI and C
  entries above; the Kotlin and Swift runtimes must match.
- **Android:** a unary call Rust makes reaches Kotlin on the Rust
  caller's thread (`onCallDirect`) while nothing queued before it is on
  its way, and the backend starts there with `CoroutineStart.UNDISPATCHED`
  (it must not block before its first suspension; opt out with
  `registerHandler(id, handler, startInPlace = false)` or
  `android_start_in_place = false`). Calls, streams, notifications and
  instance creation the pump delivers start on `Dispatchers.Default`. A
  `CancellationException` that is not the call's own cancellation (an
  uncaught `withTimeout`) answers Rust with an error instead of leaving
  the call pending, and ends a stream with an error. `IstmoRuntime.call` throws `CancellationException`
  for a cancelled caller even when the host answered in place, and takes
  `(payload, payloadLen)` (`@JvmOverloads`).
- **Android streams:** `IstmoRuntime.stream` opens a credit-based stream
  (`bufferCapacity` is the credit window): a Rust producer that runs
  ahead waits instead of overflowing the Kotlin buffer
  (`StreamOverflowException` only on a credit overrun). Consecutive
  items cross in one `onEvents` call.
- An answer to a peer call comes back from the call itself only while no
  frame queued before it (a notification the host sent first) is on its
  way; the iOS pump now reports deliveries too.
- **iOS:** `IstmoRuntime.call` calls `istmo_ios_call` from the caller's
  task, off the runtime's serial queue, and `stream` submits on the
  caller's thread too (so streams and calls reach Rust in the order they
  were made); `istmo_ios_call` answers `ERR` (with an encoded message)
  for a call it could not submit, so the continuation always resumes.

### Release notes

- `istmo-core` dev-depends on `istmo-macros` (its stream benchmark
  declares a plugin): publish `istmo-macros` before `istmo-core`.
