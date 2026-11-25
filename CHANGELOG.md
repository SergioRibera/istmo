# Changelog

## Unreleased

### Added

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

### Changed (breaking)

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

### Release notes

- `istmo-core` dev-depends on `istmo-macros` (its stream benchmark
  declares a plugin): publish `istmo-macros` before `istmo-core`.
