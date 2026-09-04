// Swift-side singleton mirroring the Kotlin `IstmoRuntime` object.
//
// Owns:
//
// * The `IstmoIosCallbacks` table pinned via `Unmanaged` so the C
//   trampolines can locate `self`.
// * A `pending` map: call_id → CheckedContinuation, resolved when the
//   matching `on_respond` callback fires.
// * A `streams` map: stream_id → AsyncThrowingStream continuation, so
//   Rust-hosted streams surface as Swift `AsyncThrowingStream<Data, Error>`.
// * A `hosts` map: pluginId → Swift-side dispatcher (deferred; the M5 demo
//   consumes a Rust-hosted plugin only, so no Swift-hosted plugin ever
//   receives an `on_call` in the demo path).
// * A `nextId` counter used when Swift initiates a call (`call(...)` /
//   `stream(...)`) — Rust owns the counter for anything it originates, but
//   Swift needs its own space for outbound calls.
//
// Everything is `@MainActor`-free — the transport thread invokes the
// callbacks; the runtime hops to a serial `DispatchQueue` internally for
// state mutations and resumes continuations on whichever executor the
// caller happens to be on.

import Foundation

public final class IstmoRuntime {

    public static let shared = IstmoRuntime()

    private let queue = DispatchQueue(label: "dev.istmo.runtime", qos: .userInitiated)
    private var started = false
    private var nextId: UInt64 = 1_000_000_000  // Swift-originated ids stay above Rust's range for debugging clarity.
    private var pending: [UInt64: CheckedContinuation<Data, Error>] = [:]
    private var streams: [UInt64: AsyncThrowingStream<Data, Error>.Continuation] = [:]
    /// Swift-side plugin dispatchers keyed by wire plugin id. Populated
    /// through `registerHandler`; consulted from `handleCall` /
    /// `handleCreateInstance` when the Rust pump delivers an inbound call
    /// for a Swift-hosted plugin.
    private var handlers: [String: PluginHandler] = [:]

    private init() {}

    // MARK: - Handler registry

    /// Register a Swift-side dispatcher for a wire plugin id. Typical
    /// call site is `main.swift` after `IstmoRuntime.shared.start()`:
    ///
    /// ```swift
    /// IstmoRuntime.shared.registerHandler(
    ///     PermissionsDispatcher.PLUGIN_ID,
    ///     PermissionsDispatcher(backend: MyPermissions(), codecs: PermissionsCodecs())
    /// )
    /// ```
    public func registerHandler(_ pluginId: String, _ handler: PluginHandler) {
        queue.sync { self.handlers[pluginId] = handler }
    }

    // MARK: - Lifecycle

    /// Starts the transport. Idempotent on the Swift side; the C side
    /// returns `false` if `Runtime::init` has already succeeded (a second
    /// `start()` would install the callback table over an already-running
    /// pump — not supported).
    public func start() throws {
        try queue.sync {
            guard !started else { return }
            let ctx = Unmanaged.passUnretained(self).toOpaque()
            let cb = IstmoIosCallbacks(
                ctx: ctx,
                on_call: Trampolines.onCall,
                on_cancel: Trampolines.onCancel,
                on_create_instance: Trampolines.onCreateInstance,
                on_destroy_instance: Trampolines.onDestroyInstance,
                on_respond: Trampolines.onRespond,
                on_event: Trampolines.onEvent,
                on_stream_end: Trampolines.onStreamEnd,
                on_release_native_handle: Trampolines.onReleaseNativeHandle
            )
            let ok = istmo_ios_start(cb)
            guard ok else { throw IstmoRuntimeError.startFailed }
            started = true
        }
    }

    /// Shuts the transport down and cancels every in-flight call / stream.
    public func shutdown() {
        queue.sync {
            istmo_ios_shutdown()
            for (_, cont) in pending {
                cont.resume(throwing: IstmoRuntimeError.shutdown)
            }
            pending.removeAll()
            for (_, cont) in streams {
                cont.finish(throwing: IstmoRuntimeError.shutdown)
            }
            streams.removeAll()
            started = false
        }
    }

    // MARK: - Unary call (Swift → Rust)

    /// Ships a bincode-encoded payload to a Rust-hosted plugin. Resolves
    /// with the encoded response bytes, or throws:
    ///
    /// * [`PluginException`] — Rust responded with `Result::Err(bytes)`.
    /// * [`IstmoRuntimeError.shutdown`] — the runtime tore down before the
    ///   response arrived.
    public func call(pluginId: String, method: String, payload: Data) async throws -> Data {
        let callId = queue.sync { () -> UInt64 in
            let id = nextId
            nextId += 1
            return id
        }
        return try await withCheckedThrowingContinuation { cont in
            queue.async {
                self.pending[callId] = cont
                let pidBytes = Array(pluginId.utf8)
                let mBytes = Array(method.utf8)
                payload.withUnsafeBytes { payloadPtr in
                    pidBytes.withUnsafeBufferPointer { pidBuf in
                        mBytes.withUnsafeBufferPointer { mBuf in
                            istmo_ios_submit_call(
                                callId,
                                pidBuf.baseAddress, pidBuf.count,
                                0,  // instance id — None
                                mBuf.baseAddress, mBuf.count,
                                payloadPtr.bindMemory(to: UInt8.self).baseAddress,
                                payload.count
                            )
                        }
                    }
                }
            }
        }
    }

    /// Void-returning convenience for fire-and-forget style callers
    /// (background task shims, notification triggers). Returns `true` if
    /// the call resolved without a domain error, `false` otherwise. Never
    /// throws — infrastructure failures are logged, not surfaced.
    public func callVoid(pluginId: String, method: String, payload: Data) async -> Bool {
        do {
            _ = try await call(pluginId: pluginId, method: method, payload: payload)
            return true
        } catch {
            NSLog("IstmoRuntime.callVoid failed: \(error)")
            return false
        }
    }

    /// Cancels every pending call for `pluginId`. Currently a coarse cancel
    /// — the runtime has no per-plugin routing table and just walks the
    /// pending map. Used by `BGTask.expirationHandler` shims.
    public func cancel(pluginId: String) {
        queue.sync {
            // TODO: index pending by plugin id once the demo has more than one plugin.
            for (_, cont) in pending {
                cont.resume(throwing: IstmoRuntimeError.shutdown)
            }
            pending.removeAll()
        }
    }

    /// Opens a stream against a Rust-hosted stream method. Each `Event`
    /// frame emits one Data value; `StreamEnd::Complete` finishes cleanly,
    /// `Cancelled` / `Error` throw the matching `IstmoRuntimeError`.
    public func stream(pluginId: String, method: String, payload: Data) -> AsyncThrowingStream<Data, Error> {
        AsyncThrowingStream { cont in
            let streamId = self.queue.sync { () -> UInt64 in
                let id = self.nextId
                self.nextId += 1
                return id
            }
            self.queue.async {
                self.streams[streamId] = cont
                let pidBytes = Array(pluginId.utf8)
                let mBytes = Array(method.utf8)
                payload.withUnsafeBytes { payloadPtr in
                    pidBytes.withUnsafeBufferPointer { pidBuf in
                        mBytes.withUnsafeBufferPointer { mBuf in
                            istmo_ios_submit_call(
                                streamId,
                                pidBuf.baseAddress, pidBuf.count,
                                0,
                                mBuf.baseAddress, mBuf.count,
                                payloadPtr.bindMemory(to: UInt8.self).baseAddress,
                                payload.count
                            )
                        }
                    }
                }
            }
            cont.onTermination = { @Sendable _ in
                // Best-effort cancel: send an empty response to signal
                // teardown. Full Cancel-frame path is a follow-up (see
                // CLAUDE.md M5 deferred: cooperative cancellation).
                self.queue.async {
                    self.streams.removeValue(forKey: streamId)
                }
            }
        }
    }

    // MARK: - Early events (Swift → Rust EarlyEventStore)

    public func publishEarlyLatest(channel: String, payload: Data) {
        let chBytes = Array(channel.utf8)
        payload.withUnsafeBytes { payloadPtr in
            chBytes.withUnsafeBufferPointer { chBuf in
                istmo_ios_submit_early_latest(
                    chBuf.baseAddress, chBuf.count,
                    payloadPtr.bindMemory(to: UInt8.self).baseAddress,
                    payload.count
                )
            }
        }
    }

    public func publishEarlyQueue(channel: String, capacity: UInt32, payload: Data) {
        let chBytes = Array(channel.utf8)
        payload.withUnsafeBytes { payloadPtr in
            chBytes.withUnsafeBufferPointer { chBuf in
                istmo_ios_submit_early_queue(
                    chBuf.baseAddress, chBuf.count,
                    capacity,
                    payloadPtr.bindMemory(to: UInt8.self).baseAddress,
                    payload.count
                )
            }
        }
    }

    // MARK: - Callback handlers (invoked from Rust pump thread)

    fileprivate func handleRespond(callId: UInt64, ok: Bool, payload: Data) {
        queue.async {
            guard let cont = self.pending.removeValue(forKey: callId) else {
                NSLog("IstmoRuntime: onRespond for unknown call_id \(callId)")
                return
            }
            if ok {
                cont.resume(returning: payload)
            } else {
                cont.resume(throwing: PluginException(payload: payload))
            }
        }
    }

    fileprivate func handleEvent(streamId: UInt64, payload: Data) {
        queue.async {
            guard let cont = self.streams[streamId] else {
                NSLog("IstmoRuntime: onEvent for unknown stream_id \(streamId)")
                return
            }
            cont.yield(payload)
        }
    }

    fileprivate func handleStreamEnd(streamId: UInt64, reason: UInt32, errPayload: Data) {
        queue.async {
            guard let cont = self.streams.removeValue(forKey: streamId) else { return }
            switch reason {
            case 0:
                cont.finish()
            case 1:
                cont.finish(throwing: IstmoRuntimeError.streamCancelled)
            case 2:
                cont.finish(throwing: IstmoRuntimeError.streamError(errPayload))
            default:
                cont.finish(throwing: IstmoRuntimeError.streamError(errPayload))
            }
        }
    }

    fileprivate func handleCall(callId: UInt64, pluginId: String, instanceId: UInt64, method: String, payload: Data) {
        let handler = queue.sync { self.handlers[pluginId] }
        guard let handler = handler else {
            NSLog("IstmoRuntime: onCall for unregistered plugin=\(pluginId)")
            istmo_ios_submit_response(callId, false, nil, 0)
            return
        }
        Task {
            do {
                let out = try await handler.handleCall(instanceId: instanceId, method: method, payload: payload)
                Self.submitResponse(callId: callId, ok: true, payload: out)
            } catch let e as PluginException {
                Self.submitResponse(callId: callId, ok: false, payload: e.payload)
            } catch {
                NSLog("IstmoRuntime: handleCall failed plugin=\(pluginId) method=\(method): \(error)")
                istmo_ios_submit_response(callId, false, nil, 0)
            }
        }
    }

    fileprivate func handleCreateInstance(callId: UInt64, pluginId: String, payload: Data) {
        let handler = queue.sync { self.handlers[pluginId] }
        guard let handler = handler else {
            NSLog("IstmoRuntime: onCreateInstance for unregistered plugin=\(pluginId)")
            istmo_ios_submit_response(callId, false, nil, 0)
            return
        }
        Task {
            do {
                let out = try await handler.handleCreateInstance(payload: payload)
                Self.submitResponse(callId: callId, ok: true, payload: out)
            } catch let e as PluginException {
                Self.submitResponse(callId: callId, ok: false, payload: e.payload)
            } catch {
                NSLog("IstmoRuntime: handleCreateInstance failed plugin=\(pluginId): \(error)")
                istmo_ios_submit_response(callId, false, nil, 0)
            }
        }
    }

    /// Ship `payload` back to Rust via `istmo_ios_submit_response`.
    /// Extracted to keep the two `handle*` methods short and consistent
    /// about how empty payloads are represented on the wire (nil ptr +
    /// zero length, matching the Rust decode fallback).
    private static func submitResponse(callId: UInt64, ok: Bool, payload: Data) {
        if payload.isEmpty {
            istmo_ios_submit_response(callId, ok, nil, 0)
        } else {
            payload.withUnsafeBytes { raw in
                let ptr = raw.bindMemory(to: UInt8.self).baseAddress
                istmo_ios_submit_response(callId, ok, ptr, payload.count)
            }
        }
    }
}

// MARK: - C trampolines

private enum Trampolines {

    static let onCall: @convention(c) (
        UnsafeMutableRawPointer?, UInt64,
        UnsafePointer<UInt8>?, Int,
        UInt64,
        UnsafePointer<UInt8>?, Int,
        UnsafePointer<UInt8>?, Int
    ) -> Void = { ctx, callId, pidPtr, pidLen, instanceId, methodPtr, methodLen, payloadPtr, payloadLen in
        guard let runtime = ctxRuntime(ctx) else { return }
        let pluginId = decodeUtf8(pidPtr, pidLen) ?? ""
        let method = decodeUtf8(methodPtr, methodLen) ?? ""
        let payload = copyData(payloadPtr, payloadLen)
        runtime.handleCall(callId: callId, pluginId: pluginId, instanceId: instanceId, method: method, payload: payload)
    }

    static let onCancel: @convention(c) (UnsafeMutableRawPointer?, UInt64) -> Void = { _, callId in
        NSLog("IstmoRuntime: onCancel call_id=\(callId) — Swift-hosted dispatch cancelled")
    }

    static let onCreateInstance: @convention(c) (
        UnsafeMutableRawPointer?, UInt64,
        UnsafePointer<UInt8>?, Int,
        UnsafePointer<UInt8>?, Int
    ) -> Void = { ctx, callId, pidPtr, pidLen, payloadPtr, payloadLen in
        guard let runtime = ctxRuntime(ctx) else { return }
        let pluginId = decodeUtf8(pidPtr, pidLen) ?? ""
        let payload = copyData(payloadPtr, payloadLen)
        runtime.handleCreateInstance(callId: callId, pluginId: pluginId, payload: payload)
    }

    static let onDestroyInstance: @convention(c) (UnsafeMutableRawPointer?, UInt64) -> Void = { _, instanceId in
        NSLog("IstmoRuntime: onDestroyInstance instance_id=\(instanceId) — no factory registered")
    }

    static let onRespond: @convention(c) (
        UnsafeMutableRawPointer?, UInt64, Bool,
        UnsafePointer<UInt8>?, Int
    ) -> Void = { ctx, callId, ok, payloadPtr, payloadLen in
        guard let runtime = ctxRuntime(ctx) else { return }
        runtime.handleRespond(callId: callId, ok: ok, payload: copyData(payloadPtr, payloadLen))
    }

    static let onEvent: @convention(c) (
        UnsafeMutableRawPointer?, UInt64,
        UnsafePointer<UInt8>?, Int
    ) -> Void = { ctx, streamId, payloadPtr, payloadLen in
        guard let runtime = ctxRuntime(ctx) else { return }
        runtime.handleEvent(streamId: streamId, payload: copyData(payloadPtr, payloadLen))
    }

    static let onStreamEnd: @convention(c) (
        UnsafeMutableRawPointer?, UInt64, UInt32,
        UnsafePointer<UInt8>?, Int
    ) -> Void = { ctx, streamId, reason, payloadPtr, payloadLen in
        guard let runtime = ctxRuntime(ctx) else { return }
        runtime.handleStreamEnd(streamId: streamId, reason: reason, errPayload: copyData(payloadPtr, payloadLen))
    }

    static let onReleaseNativeHandle: @convention(c) (
        UnsafeMutableRawPointer?, UInt64
    ) -> Void = { _, handleId in
        // Demo runtime does not own any native handles yet. Real apps hop
        // to the main queue and free the object stored under `handleId` in
        // their per-plugin registry.
        NSLog("IstmoRuntime: onReleaseNativeHandle handle_id=\(handleId) — no handle registry")
    }

    private static func ctxRuntime(_ ctx: UnsafeMutableRawPointer?) -> IstmoRuntime? {
        guard let ctx = ctx else { return nil }
        return Unmanaged<IstmoRuntime>.fromOpaque(ctx).takeUnretainedValue()
    }

    private static func decodeUtf8(_ ptr: UnsafePointer<UInt8>?, _ len: Int) -> String? {
        guard let ptr = ptr, len > 0 else { return "" }
        let bytes = UnsafeBufferPointer(start: ptr, count: len)
        return String(bytes: bytes, encoding: .utf8)
    }

    private static func copyData(_ ptr: UnsafePointer<UInt8>?, _ len: Int) -> Data {
        guard let ptr = ptr, len > 0 else { return Data() }
        return Data(bytes: ptr, count: len)
    }
}
