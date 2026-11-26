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
- Direct paths between Rust and the platform:
  `Dispatch::method_mode`; `Runtime::serve_peer_call` (an inline host's
  answer comes back from the call itself), `serve_peer_stream` +
  `grant_stream_credit` (credit-based peer streams);
  `DirectPeer` / `DirectOutcome` + `Runtime::install_direct_peer`,
  `outbound_delivered`, `track_outbound_deliveries` (Rust → Kotlin calls
  and stream items on the caller's thread); `DeliveryScope` (a thread
  that must never block on the outbound channel).
- JNI `nativeCall`, `nativeSubmitStream`, `nativeStreamCredit`,
  `nativeMayBlock`; Kotlin callbacks `onCallDirect` and `onEvents`; C
  `istmo_ios_call` + `IstmoIosReply` + `istmo_ios_free_reply`.
- `istmo_core::spin::SpinWait` (bounded adaptive spinning before a park,
  `spin::set_enabled` for A/B); `native_frame::BytePool` (recycled CPU
  frame buffers); Kotlin `IstmoBytesOut`.
- `istmo.toml` `android_start_in_place = false`: the generated registry
  registers that plugin's backend with `startInPlace = false`.
- `Runtime::outbound_spill()` (`SpillStats`: frames queued behind a full
  outbound channel), `BlockingThreads::now()` (blocking workers alive
  and their peak; a warning past 256), `DeliveryScope::may_block()` and
  JNI `nativeMayBlock`.

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
