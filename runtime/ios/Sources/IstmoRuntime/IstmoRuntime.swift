import Foundation
import IstmoFFI

/// Every mutable property is read and written only on `queue` (serial),
/// which is what makes sharing the runtime across threads sound.
public final class IstmoRuntime: @unchecked Sendable {

    public static let shared = IstmoRuntime()

    private let queue = DispatchQueue(label: "dev.istmo.runtime", qos: .userInitiated)
    private var started = false
    /// The `ctx` handed to Rust holds a strong reference so a callback can
    /// never observe a freed runtime; released once the pump is joined.
    private var retainedCtx: Unmanaged<IstmoRuntime>?
    private var nextId: UInt64 = 1_000_000_000
    /// Calls into Rust awaiting their response, keyed by call id. The
    /// plugin id lets `cancel(pluginId:)` pick out its own calls.
    private var pending: [UInt64: PendingCall] = [:]
    private var streams: [UInt64: PendingStream] = [:]

    private struct PendingCall {
        let pluginId: String
        let cont: CheckedContinuation<Data, Error>
    }

    private struct PendingStream {
        let pluginId: String
        let cont: AsyncThrowingStream<Data, Error>.Continuation
    }

    private var handlers: [String: PluginHandler] = [:]

    /// Swift-hosted calls in flight, so a Rust-side cancel (the caller
    /// dropped its future) can cancel the backend's `Task`.
    private var hostedTasks: [UInt64: Task<Void, Never>] = [:]

    private var handleOwners: [UInt64: String] = [:]
    private var appObjects: [UInt64: AnyObject] = [:]
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

    /// Register a platform object app code hands to a plugin (an
    /// `AVSampleBufferDisplayLayer` for a preview, a consent grant) and get
    /// the `NativeHandleId` to pass as the plugin argument. The plugin's
    /// backend reads it back with `object(for:)`; it is released when
    /// Rust drops its `NativeHandle`, or with `releaseObject(_:)`.
    public func registerObject(_ object: AnyObject) -> UInt64 {
        queue.sync {
            let id = self.nextGlobalHandleId
            self.nextGlobalHandleId += 1
            self.appObjects[id] = object
            return id
        }
    }

    /// The object `registerObject(_:)` stored under `handleId`, if it is a `T`.
    public func object<T: AnyObject>(for handleId: UInt64, as type: T.Type = T.self) -> T? {
        queue.sync { self.appObjects[handleId] as? T }
    }

    /// Forget an object `registerObject(_:)` stored.
    public func releaseObject(_ handleId: UInt64) {
        queue.sync { _ = self.appObjects.removeValue(forKey: handleId) }
    }

    public func registerHandler(_ pluginId: String, _ handler: PluginHandler) {
        queue.sync { self.handlers[pluginId] = handler }
    }

    public func start() throws {
        try queue.sync {
            guard !started else { return }
            if let found = IstmoFrames.abiMismatch() {
                throw IstmoRuntimeError.abiMismatch(found: found, expected: IstmoFrames.abiVersion)
            }
            let retained = Unmanaged.passRetained(self)
            let cb = IstmoIosCallbacks(
                ctx: retained.toOpaque(),
                on_call: Trampolines.onCall,
                on_cancel: Trampolines.onCancel,
                on_create_instance: Trampolines.onCreateInstance,
                on_destroy_instance: Trampolines.onDestroyInstance,
                on_respond: Trampolines.onRespond,
                on_event: Trampolines.onEvent,
                on_stream_end: Trampolines.onStreamEnd,
                on_release_native_handle: Trampolines.onReleaseNativeHandle,
                on_notify: Trampolines.onNotify
            )
            guard istmo_ios_start(cb) else {
                retained.release()
                throw IstmoRuntimeError.startFailed
            }
            retainedCtx = retained
            started = true
        }
    }

    public func shutdown() {
        let wasStarted = queue.sync { () -> Bool in
            let was = started
            started = false
            return was
        }
        guard wasStarted else { return }
        // Outside `queue`: the join waits for the in-flight callback, and
        // callbacks themselves hop onto `queue`.
        if istmo_ios_shutdown() {
            // Joined: no callback can still be using `ctx`. Otherwise
            // (shutdown from inside a callback) the reference is leaked
            // on purpose rather than freed under a running callback.
            queue.sync {
                retainedCtx?.release()
                retainedCtx = nil
            }
        }
        queue.sync {
            for (_, call) in pending {
                call.cont.resume(throwing: IstmoRuntimeError.shutdown)
            }
            pending.removeAll()
            for (_, stream) in streams {
                stream.cont.finish(throwing: IstmoRuntimeError.shutdown)
            }
            streams.removeAll()
            for (_, task) in hostedTasks {
                task.cancel()
            }
            hostedTasks.removeAll()
        }
    }

    public func call(pluginId: String, method: String, payload: Data) async throws -> Data {
        let callId = queue.sync { () -> UInt64 in
            let id = nextId
            nextId += 1
            return id
        }
        return try await withCheckedThrowingContinuation { cont in
            // Outside start()..shutdown() no pump delivers a response, so
            // the continuation would never resume. `shutdown()` fails
            // whatever got in before `started` flipped. Registered first:
            // a host that waits answers through `on_respond`, which looks
            // the call up here.
            let registered = queue.sync { () -> Bool in
                guard self.started else { return false }
                self.pending[callId] = PendingCall(pluginId: pluginId, cont: cont)
                return true
            }
            guard registered else {
                cont.resume(throwing: IstmoRuntimeError.shutdown)
                return
            }
            // Off `queue`, on the caller's thread: an inline Rust host runs
            // inside this call, and the pump's callbacks take `queue`
            // synchronously, so a host that waits for the pump must not
            // hold it.
            let pidBytes = Array(pluginId.utf8)
            let mBytes = Array(method.utf8)
            var reply = IstmoIosReply(ptr: nil, len: 0, cap: 0)
            let status = payload.withUnsafeBytes { payloadPtr in
                pidBytes.withUnsafeBufferPointer { pidBuf in
                    mBytes.withUnsafeBufferPointer { mBuf in
                        istmo_ios_call(
                            callId,
                            pidBuf.baseAddress, pidBuf.count,
                            0,
                            mBuf.baseAddress, mBuf.count,
                            payloadPtr.bindMemory(to: UInt8.self).baseAddress,
                            payload.count,
                            &reply
                        )
                    }
                }
            }
            // The answer comes as a frame through `on_respond`.
            guard status != Int32(ISTMO_IOS_CALL_PENDING) else { return }
            // Answered here (or refused before it was submitted): no frame
            // follows.
            let answer = reply.ptr.map { Data(bytes: $0, count: reply.len) } ?? Data()
            istmo_ios_free_reply(&reply)
            let waiting = queue.sync { self.pending.removeValue(forKey: callId)?.cont }
            guard let waiting = waiting else { return }
            if status == Int32(ISTMO_IOS_CALL_OK) {
                waiting.resume(returning: answer)
            } else {
                waiting.resume(throwing: PluginException(payload: answer))
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

    /// Cancel every call and stream this runtime has in flight to
    /// `pluginId`: each fails locally with `IstmoRuntimeError.cancelled` /
    /// `.streamCancelled`, and Rust gets a cooperative cancel so the
    /// hosted work stops too. Calls to other plugins are left alone.
    public func cancel(pluginId: String) {
        let callIds = queue.sync { () -> [UInt64] in
            var ids: [UInt64] = []
            for (id, call) in pending where call.pluginId == pluginId {
                pending.removeValue(forKey: id)
                call.cont.resume(throwing: IstmoRuntimeError.cancelled)
                ids.append(id)
            }
            for (id, stream) in streams where stream.pluginId == pluginId {
                streams.removeValue(forKey: id)
                stream.cont.finish(throwing: IstmoRuntimeError.streamCancelled)
                ids.append(id)
            }
            return ids
        }
        // Outside `queue`: the cancel is delivered on Rust's side and
        // nothing here needs the runtime's state any more.
        for id in callIds {
            istmo_ios_submit_cancel(id)
        }
    }

    public func stream(pluginId: String, method: String, payload: Data) -> AsyncThrowingStream<Data, Error> {
        AsyncThrowingStream { cont in
            let streamId = self.queue.sync { () -> UInt64 in
                let id = self.nextId
                self.nextId += 1
                return id
            }
            let registered = self.queue.sync { () -> Bool in
                guard self.started else { return false }
                self.streams[streamId] = PendingStream(pluginId: pluginId, cont: cont)
                return true
            }
            guard registered else {
                cont.finish(throwing: IstmoRuntimeError.shutdown)
                return
            }
            cont.onTermination = { @Sendable _ in
                self.queue.async {
                    // Still registered means the consumer stopped first
                    // (every other end removes the entry before finishing),
                    // so Rust has to stop producing. A cancel that beats
                    // the submit below is kept until the stream arrives.
                    if self.streams.removeValue(forKey: streamId) != nil {
                        istmo_ios_submit_cancel(streamId)
                    }
                }
            }
            // Off `queue` (see `call`), on the caller's thread like `call`:
            // an inline Rust host starts its stream inside the submit, and
            // submitting here keeps the caller's order (a stream opened
            // before a call or another stream reaches Rust first).
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
    }

    /// Fire-and-forget call to a Rust-hosted plugin: no response comes
    /// back and failures are only logged on the Rust side.
    /// `instanceId` 0 targets the stateless plugin.
    public func notify(pluginId: String, instanceId: UInt64 = 0, method: String, payload: Data) {
        let pidBytes = Array(pluginId.utf8)
        let mBytes = Array(method.utf8)
        payload.withUnsafeBytes { payloadPtr in
            pidBytes.withUnsafeBufferPointer { pidBuf in
                mBytes.withUnsafeBufferPointer { mBuf in
                    istmo_ios_submit_notify(
                        pidBuf.baseAddress, pidBuf.count,
                        instanceId,
                        mBuf.baseAddress, mBuf.count,
                        payloadPtr.bindMemory(to: UInt8.self).baseAddress,
                        payload.count
                    )
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
            guard let cont = self.pending.removeValue(forKey: callId)?.cont else {
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
            guard let cont = self.streams[streamId]?.cont else {
                NSLog("IstmoRuntime: onEvent for unknown stream_id \(streamId)")
                return
            }
            cont.yield(payload)
        }
    }

    fileprivate func handleStreamEnd(streamId: UInt64, reason: UInt32, errPayload: Data) {
        queue.async {
            guard let cont = self.streams.removeValue(forKey: streamId)?.cont else { return }
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
        spawnHosted(callId: callId) {
            try await handler.handleCall(instanceId: instanceId, method: method, payload: payload)
        } onFailure: { error in
            NSLog("IstmoRuntime: handleCall failed plugin=\(pluginId) method=\(method): \(error)")
        }
    }

    /// No response and no cancel token: Rust never tracks a notify, so the
    /// task is not registered in `hostedTasks`.
    fileprivate func handleNotify(pluginId: String, instanceId: UInt64, method: String, payload: Data) {
        let handler = queue.sync { self.handlers[pluginId] }
        guard let handler = handler else {
            NSLog("IstmoRuntime: onNotify for unregistered plugin=\(pluginId)")
            return
        }
        Task {
            await handler.handleNotify(instanceId: instanceId, method: method, payload: payload)
        }
    }

    fileprivate func handleCreateInstance(callId: UInt64, pluginId: String, payload: Data) {
        let handler = queue.sync { self.handlers[pluginId] }
        guard let handler = handler else {
            NSLog("IstmoRuntime: onCreateInstance for unregistered plugin=\(pluginId)")
            istmo_ios_submit_response(callId, false, nil, 0)
            return
        }
        spawnHosted(callId: callId) {
            .unary(try await handler.handleCreateInstance(payload: payload))
        } onFailure: { error in
            NSLog("IstmoRuntime: handleCreateInstance failed plugin=\(pluginId): \(error)")
        }
    }

    /// Cooperative cancel: the backend observes it through
    /// `Task.isCancelled` / `withTaskCancellationHandler`.
    fileprivate func handleCancel(callId: UInt64) {
        let task = queue.sync { self.hostedTasks.removeValue(forKey: callId) }
        task?.cancel()
    }

    /// Run a Swift-hosted call in a `Task` registered under `callId`.
    /// Cancelled calls get no response: Rust already forgot the call id.
    private func spawnHosted(
        callId: UInt64,
        _ body: @escaping () async throws -> PluginResult,
        onFailure: @escaping (Error) -> Void
    ) {
        // Registering inside the serial queue guarantees the task's own
        // removal below runs after the insertion.
        queue.sync {
            self.hostedTasks[callId] = Task {
                defer { self.queue.async { self.hostedTasks.removeValue(forKey: callId) } }
                do {
                    switch try await body() {
                    case .unary(let out):
                        guard !Task.isCancelled else { return }
                        Self.submitResponse(callId: callId, ok: true, payload: out)
                    case .stream(let items):
                        await Self.pumpStream(streamId: callId, items)
                    }
                } catch let e as PluginException {
                    guard !Task.isCancelled else { return }
                    Self.submitResponse(callId: callId, ok: false, payload: e.payload)
                } catch {
                    guard !Task.isCancelled else { return }
                    onFailure(error)
                    istmo_ios_submit_response(callId, false, nil, 0)
                }
            }
        }
    }

    fileprivate func handleReleaseNativeHandle(handleId: UInt64) {
        if queue.sync(execute: { self.appObjects.removeValue(forKey: handleId) }) != nil { return }
        let owner = queue.sync { self.handleOwners.removeValue(forKey: handleId) }
        guard let ownerId = owner, let handler = queue.sync(execute: { self.handlers[ownerId] }) else {
            return
        }
        if let releaser = handler as? HandleReleaser {
            releaser.releaseNativeHandle(handleId)
        }
    }

    /// Forwards a hosted stream to Rust: one `Event` per item, then a
    /// `StreamEnd`. The stream id is the call id. A cancelled task (Rust
    /// dropped the stream) sends nothing more: Rust already forgot the id.
    private static func pumpStream(streamId: UInt64, _ items: AsyncThrowingStream<Data, Error>) async {
        do {
            for try await item in items {
                guard !Task.isCancelled else { return }
                submitEvent(streamId: streamId, payload: item)
            }
            guard !Task.isCancelled else { return }
            submitStreamEnd(streamId: streamId, reason: streamEndComplete, payload: Data())
        } catch let e as PluginException {
            guard !Task.isCancelled else { return }
            submitStreamEnd(streamId: streamId, reason: streamEndError, payload: e.payload)
        } catch {
            guard !Task.isCancelled else { return }
            NSLog("IstmoRuntime: hosted stream \(streamId) failed: \(error)")
            submitStreamEnd(streamId: streamId, reason: streamEndCancelled, payload: Data())
        }
    }

    private static let streamEndComplete: UInt32 = 0
    private static let streamEndCancelled: UInt32 = 1
    private static let streamEndError: UInt32 = 2

    private static func submitEvent(streamId: UInt64, payload: Data) {
        if payload.isEmpty {
            istmo_ios_submit_event(streamId, nil, 0)
        } else {
            payload.withUnsafeBytes { raw in
                let ptr = raw.bindMemory(to: UInt8.self).baseAddress
                istmo_ios_submit_event(streamId, ptr, payload.count)
            }
        }
    }

    private static func submitStreamEnd(streamId: UInt64, reason: UInt32, payload: Data) {
        if payload.isEmpty {
            istmo_ios_submit_stream_end(streamId, reason, nil, 0)
        } else {
            payload.withUnsafeBytes { raw in
                let ptr = raw.bindMemory(to: UInt8.self).baseAddress
                istmo_ios_submit_stream_end(streamId, reason, ptr, payload.count)
            }
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
        guard let runtime = ctxRuntime(ctx) else { return }
        runtime.handleCancel(callId: callId)
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

    static let onNotify: @convention(c) (
        UnsafeMutableRawPointer?,
        UnsafePointer<UInt8>?, Int,
        UInt64,
        UnsafePointer<UInt8>?, Int,
        UnsafePointer<UInt8>?, Int
    ) -> Void = { ctx, pidPtr, pidLen, instanceId, methodPtr, methodLen, payloadPtr, payloadLen in
        guard let runtime = ctxRuntime(ctx) else { return }
        let pluginId = decodeUtf8(pidPtr, pidLen) ?? ""
        let method = decodeUtf8(methodPtr, methodLen) ?? ""
        let payload = copyData(payloadPtr, payloadLen)
        runtime.handleNotify(pluginId: pluginId, instanceId: instanceId, method: method, payload: payload)
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

