import Foundation
import IstmoRuntime
#if canImport(ActivityKit)
import ActivityKit
#endif

public final class LiveActivityBackendImpl: LiveActivityBackend, HandleReleaser {

    private let queue = DispatchQueue(label: "dev.istmo.plugins.live-activity.backend")

    private var handlersByType: [String: LiveActivityHandler] = [:]
    private var typeByHandle: [UInt64: String] = [:]

    public init() {}

    public func register(handler: LiveActivityHandler) {
        queue.sync {
            handlersByType[handler.activityType] = handler
        }
    }

    public func unregister(activityType: String) {
        queue.sync {
            _ = handlersByType.removeValue(forKey: activityType)
        }
    }

    public func start(
        activity_type: String,
        attributes: Data,
        initial_state: Data,
        style: ActivityStyle,
        stale_after_seconds: UInt32?,
        android_tier_hint _: AndroidTierHint?
    ) async throws -> NativeHandleId {
        let handler = try handler(for: activity_type)
        let id = try await handler.start(
            attributes: attributes,
            initialState: initial_state,
            style: style,
            staleAfterSeconds: stale_after_seconds,
            androidTierHint: nil
        )
        queue.sync {
            typeByHandle[id.value] = activity_type
        }
        return id
    }

    public func update(
        handle: NativeHandleId,
        state: Data,
        alert: AlertConfig?
    ) async throws {
        let handler = try handler(forHandle: handle)
        try await handler.update(handle: handle, state: state, alert: alert)
    }

    public func end(
        handle: NativeHandleId,
        final_state: Data?,
        dismissal: DismissalPolicy
    ) async throws {
        let handler = try handler(forHandle: handle)
        try await handler.end(handle: handle, finalState: final_state, dismissal: dismissal)
        queue.sync {
            _ = typeByHandle.removeValue(forKey: handle.value)
        }
    }

    public func are_activities_enabled() async throws -> Bool {
        #if canImport(ActivityKit)
        if #available(iOS 16.1, *) {
            return ActivityAuthorizationInfo().areActivitiesEnabled
        }
        return false
        #else
        return false
        #endif
    }

    public func capabilities() async throws -> PlatformCapabilities {
        #if canImport(ActivityKit)
        if #available(iOS 16.1, *) {
            let info = ActivityAuthorizationInfo()
            return .ios(
                IosCapabilities(
                    activity_kit_available: true,
                    activities_enabled: info.areActivitiesEnabled,

                    dynamic_island: true,
                    push_updates: false
                )
            )
        }
        return .ios(
            IosCapabilities(
                activity_kit_available: false,
                activities_enabled: false,
                dynamic_island: false,
                push_updates: false
            )
        )
        #else
        return .unsupported
        #endif
    }

    public func restore_active() async throws -> [RestoredActivity] {
        let handlers = queue.sync { Array(handlersByType.values) }
        var out: [RestoredActivity] = []
        for handler in handlers {
            let restored = try await handler.restoreActive()
            for item in restored {
                queue.sync {
                    typeByHandle[item.handle.value] = handler.activityType
                }
                out.append(item)
            }
        }
        return out
    }

    public func releaseNativeHandle(_ handleId: UInt64) {
        let ty = queue.sync { typeByHandle[handleId] }
        guard let ty, let handler = queue.sync(execute: { handlersByType[ty] }) else {
            return
        }
        handler.releaseHandle(NativeHandleId(value: handleId))
        queue.sync {
            _ = typeByHandle.removeValue(forKey: handleId)
        }
    }

    private func handler(for activityType: String) throws -> LiveActivityHandler {
        let handler = queue.sync { handlersByType[activityType] }
        guard let handler else {
            throw ActivityError.unknownActivityType(activityType)
        }
        return handler
    }

    private func handler(forHandle handle: NativeHandleId) throws -> LiveActivityHandler {
        let ty = queue.sync { typeByHandle[handle.value] }
        guard let ty else {
            throw ActivityError.handleNotFound
        }
        return try handler(for: ty)
    }
}

