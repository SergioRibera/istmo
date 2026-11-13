// Stand-in for the BackgroundTasks API the generated background shims use.

import Foundation

open class BGTask {
    public var expirationHandler: (() -> Void)?
    public func setTaskCompleted(success: Bool) {}
}
public final class BGAppRefreshTask: BGTask {}
public final class BGProcessingTask: BGTask {}
open class BGTaskRequest {
    public var earliestBeginDate: Date?
    public init(identifier: String) {}
}
public final class BGAppRefreshTaskRequest: BGTaskRequest {}
public final class BGProcessingTaskRequest: BGTaskRequest {
    public var requiresExternalPower = false
    public var requiresNetworkConnectivity = false
}
public final class BGTaskScheduler {
    public static let shared = BGTaskScheduler()
    @discardableResult
    public func register(forTaskWithIdentifier: String, using: Int?, launchHandler: @escaping (BGTask) -> Void) -> Bool { true }
    public func submit(_ request: BGTaskRequest) throws {}
}
