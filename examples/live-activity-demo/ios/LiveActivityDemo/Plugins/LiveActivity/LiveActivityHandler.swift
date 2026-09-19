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

    func restoreActive() async throws -> [RestoredActivity]

    func releaseHandle(_ handle: NativeHandleId)
}

public extension LiveActivityHandler {

    func restoreActive() async throws -> [RestoredActivity] { [] }
}

