import Foundation
import IstmoRuntime

/// Type-erased handler surface consumed by ``LiveActivityBackendImpl``.
///
/// One implementation lives on the consumer app side per activity type
/// (`"timer"`, `"delivery"`, `"workout"`). The base backend routes each
/// wire call to the handler registered for its ``activity_type`` and
/// tracks the ``NativeHandleId`` ↔ activity-type mapping.
///
/// Authors most commonly build handlers on top of ``ActivityKitLiveActivityHandler``
/// which supplies the ``ActivityKit`` plumbing over a concrete
/// ``ActivityAttributes`` conformer. Handlers whose UI is bespoke (custom
/// widgets, Live Activity + extra scheduling) can implement this protocol
/// directly.
///
/// Every method receives / returns the raw bincode-encoded payloads
/// negotiated with the Rust plugin surface; the handler is responsible
/// for decoding into its concrete `Attributes` / `ContentState` types.
public protocol LiveActivityHandler: AnyObject {

    /// Return `true` when this handler wishes to service the given
    /// ``activity_type``. The default routing key is a string match, so
    /// most handlers can return `type == self.activityType`.
    var activityType: String { get }

    /// Start a new activity. Return the fresh ``NativeHandleId`` — the
    /// base backend registers it in its handle map so future
    /// ``update``/``end`` calls route back to this handler.
    ///
    /// Throw ``ActivityError`` for typed failures (`.notSupported`,
    /// `.disabled`, `.exceededMaximum`, `.decode`, `.backend`). Any other
    /// error is wrapped into `.backend(String(describing: error))`.
    func start(
        attributes: Data,
        initialState: Data,
        style: ActivityStyle,
        staleAfterSeconds: UInt32?,
        androidTierHint: AndroidTierHint?
    ) async throws -> NativeHandleId

    func update(
        handle: NativeHandleId,
        state: Data,
        alert: AlertConfig?
    ) async throws

    func end(
        handle: NativeHandleId,
        finalState: Data?,
        dismissal: DismissalPolicy
    ) async throws

    /// Restore activities of this type that survived a process restart.
    /// Return an empty array when nothing was persisted.
    func restoreActive() async throws -> [RestoredActivity]

    /// Called by the base backend when the Rust side drops the
    /// ``NativeHandle``. Handlers should treat the release as an
    /// immediate end (equivalent to ``end`` with `.immediate`).
    func releaseHandle(_ handle: NativeHandleId)
}

public extension LiveActivityHandler {
    /// Default: no restored activities.
    func restoreActive() async throws -> [RestoredActivity] { [] }
}
