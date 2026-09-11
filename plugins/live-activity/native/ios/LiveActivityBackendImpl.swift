import Foundation
import IstmoRuntime
#if canImport(ActivityKit)
import ActivityKit
#endif

/// Reference `LiveActivityBackend` implementation.
///
/// Routes every wire call to a per-activity-type ``LiveActivityHandler``
/// registered by the consumer app. The plugin ships this class so a
/// typical app only writes the concrete handler(s) for its own activity
/// types — the routing, handle registry and capability probes come for
/// free.
///
/// Wiring in the app's Swift entry point:
///
/// ```swift
/// let backend = LiveActivityBackendImpl()
/// backend.register(handler: TimerLiveActivityHandler())
///
/// IstmoRuntime.shared.registerHandler(
///     LiveActivityDispatcher.PLUGIN_ID,
///     LiveActivityDispatcher(backend: backend, codecs: LiveActivityCodecsImpl())
/// )
/// ```
public final class LiveActivityBackendImpl: LiveActivityBackend, HandleReleaser {

    /// Serial queue guarding the routing tables. Every dictionary
    /// mutation happens on this queue; `async` methods hop off it before
    /// awaiting the handler.
    private let queue = DispatchQueue(label: "dev.istmo.plugins.live-activity.backend")

    private var handlersByType: [String: LiveActivityHandler] = [:]
    private var typeByHandle: [UInt64: String] = [:]

    public init() {}

    // ---- Registration ----------------------------------------------------

    /// Register a handler for its declared ``activityType``. Overwrites
    /// any prior registration under the same key.
    public func register(handler: LiveActivityHandler) {
        queue.sync {
            handlersByType[handler.activityType] = handler
        }
    }

    /// Remove the handler previously registered for ``activityType``.
    /// Handles currently owned by the removed handler are left in the
    /// map so an in-flight `update`/`end` still routes correctly; the
    /// consumer is responsible for their eventual release.
    public func unregister(activityType: String) {
        queue.sync {
            _ = handlersByType.removeValue(forKey: activityType)
        }
    }

    // ---- LiveActivityBackend --------------------------------------------

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
            androidTierHint: nil // iOS ignores the Android hint entirely.
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
                    // Dynamic Island can only be probed via a live
                    // Activity; treat every device on iOS 16.1+ as
                    // "may support" and let SwiftUI's `.dynamicIsland`
                    // configuration handle the visual fallback.
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

    // ---- HandleReleaser --------------------------------------------------

    /// Invoked by the dispatcher when the Rust side drops its
    /// `NativeHandle<LiveActivityToken>`. Unknown handles no-op — the
    /// contract with the runtime is that release races are tolerated.
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

    // ---- Internal --------------------------------------------------------

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
