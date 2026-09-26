import Foundation

public final class IstmoRuntime {

    public static let shared = IstmoRuntime()

    private let queue = DispatchQueue(label: "dev.istmo.runtime", qos: .userInitiated)
    private var started = false
    private var nextId: UInt64 = 1_000_000_000
    private var pending: [UInt64: CheckedContinuation<Data, Error>] = [:]
    private var streams: [UInt64: AsyncThrowingStream<Data, Error>.Continuation] = [:]

    private var handlers: [String: PluginHandler] = [:]

    /// Swift-hosted calls in flight, so `onCancel` can cancel them.
    private var inFlight: [UInt64: Task<Void, Never>] = [:]

    private var handleOwners: [UInt64: String] = [:]
    private var nextGlobalHandleId: UInt64 = 1

    private init() {}

    public func allocHandleId(pluginId: String) -> UInt64 {
        queue.sync {
            let id = self.nextGlobalHandleId
            self.nextGlobalHandleId += 1
            self.handleOwners[id] = pluginId
            return id
        }
    }

    public func forgetHandle(_ handleId: UInt64) {
        queue.sync { _ = self.handleOwners.removeValue(forKey: handleId) }
    }

    public func registerHandler(_ pluginId: String, _ handler: PluginHandler) {
        queue.sync { self.handlers[pluginId] = handler }
    }

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
                                0,
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

    public func callVoid(pluginId: String, method: String, payload: Data) async -> Bool {
        do {
            _ = try await call(pluginId: pluginId, method: method, payload: payload)
            return true
        } catch {
            NSLog("IstmoRuntime.callVoid failed: \(error)")
            return false
        }
    }

    public func cancel(pluginId: String) {
        queue.sync {

            for (_, cont) in pending {
                cont.resume(throwing: IstmoRuntimeError.shutdown)
            }
            pending.removeAll()
        }
    }

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

                self.queue.async {
                    self.streams.removeValue(forKey: streamId)
                }
            }
        }
    }

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
        let task = Task {
            defer { self.queue.async { self.inFlight.removeValue(forKey: callId) } }
            do {
                let out = try await handler.handleCall(instanceId: instanceId, method: method, payload: payload)
                Self.submitResponse(callId: callId, ok: true, payload: out)
            } catch let e as PluginException {
                Self.submitResponse(callId: callId, ok: false, payload: e.payload)
            } catch is CancellationError {
                // The Rust caller dropped the call; nobody awaits a reply.
            } catch {
                NSLog("IstmoRuntime: handleCall failed plugin=\(pluginId) method=\(method): \(error)")
                istmo_ios_submit_response(callId, false, nil, 0)
            }
        }
        queue.async { self.inFlight[callId] = task }
    }

    /// Cancel the Swift `Task` serving `callId`. Backends observe it
    /// through `Task.isCancelled` / `withTaskCancellationHandler`.
    fileprivate func handleCancel(callId: UInt64) {
        queue.async {
            self.inFlight.removeValue(forKey: callId)?.cancel()
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

    fileprivate func handleReleaseNativeHandle(handleId: UInt64) {
        let owner = queue.sync { self.handleOwners.removeValue(forKey: handleId) }
        guard let ownerId = owner, let handler = queue.sync({ self.handlers[ownerId] }) else {
            return
        }
        if let releaser = handler as? HandleReleaser {
            releaser.releaseNativeHandle(handleId)
        }
    }

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

    static let onCancel: @convention(c) (UnsafeMutableRawPointer?, UInt64) -> Void = { ctx, callId in
        ctxRuntime(ctx)?.handleCancel(callId: callId)
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
    ) -> Void = { ctx, handleId in
        guard let runtime = ctxRuntime(ctx) else { return }
        runtime.handleReleaseNativeHandle(handleId: handleId)
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

