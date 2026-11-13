import Foundation
import IstmoRuntime

public protocol LiveActivityHandler: AnyObject {

    var activityType: String { get }

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

    /// Activities of this type still running. One that already has a
    /// handle (from `start` or an earlier call) must be reported under
    /// that handle, never a newly allocated one: `releaseHandle` ends the
    /// activity, so two handles would let the first drop end it while
    /// the other is still in use.
    func restoreActive() async throws -> [RestoredActivity]

    /// The Rust owner dropped `handle`: end its activity and forget it.
    func releaseHandle(_ handle: NativeHandleId)
}

public extension LiveActivityHandler {

    func restoreActive() async throws -> [RestoredActivity] { [] }
}

