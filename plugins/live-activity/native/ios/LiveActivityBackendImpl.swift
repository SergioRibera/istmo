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
            typeByHandle[id] = activity_type
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
            _ = typeByHandle.removeValue(forKey: handle)
        }
        IstmoRuntime.shared.forgetHandle(handle)
    }

    public func are_activities_enabled() async throws -> Bool {
        #if canImport(ActivityKit)
        if #available(iOS 16.2, *) {
            return ActivityAuthorizationInfo().areActivitiesEnabled
        }
        return false
        #else
        return false
        #endif
    }

    public func capabilities() async throws -> PlatformCapabilities {
        #if canImport(ActivityKit)
        if #available(iOS 16.2, *) {
            let info = ActivityAuthorizationInfo()
            return .ios(
                IosCapabilities(
                    activityKitAvailable: true,
                    activitiesEnabled: info.areActivitiesEnabled,
                    pushUpdates: false
                )
            )
        }
        return .ios(
            IosCapabilities(
                activityKitAvailable: false,
                activitiesEnabled: false,
                pushUpdates: false
            )
        )
        #else
        return .unsupported
        #endif
    }

    // Only the requested type's handler runs: every id it allocates is
    // returned to a Rust adapter that adopts (and later releases) it.
    public func restore_active(activity_type: String) async throws -> [RestoredActivity] {
        let handler = try handler(for: activity_type)
        let restored = try await handler.restoreActive()
        queue.sync {
            for item in restored {
                typeByHandle[item.handle] = handler.activityType
            }
        }
        return restored
    }

    public func releaseNativeHandle(_ handleId: UInt64) {
        let ty = queue.sync { typeByHandle[handleId] }
        guard let ty, let handler = queue.sync(execute: { handlersByType[ty] }) else {
            return
        }
        handler.releaseHandle(handleId)
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
        let ty = queue.sync { typeByHandle[handle] }
        guard let ty else {
            throw ActivityError.handleNotFound
        }
        return try handler(for: ty)
    }
}

