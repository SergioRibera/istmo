// Stand-in for the UserNotifications slice the notifications native
// backend uses.

import Foundation

public struct UNNotificationSoundName: Hashable, Sendable {
    public let rawValue: String
    public init(rawValue: String) { self.rawValue = rawValue }
}

public let UNNotificationDefaultActionIdentifier = "com.apple.UNNotificationDefaultActionIdentifier"
public let UNNotificationDismissActionIdentifier = "com.apple.UNNotificationDismissActionIdentifier"

public struct UNAuthorizationOptions: OptionSet, Sendable {
    public let rawValue: UInt
    public init(rawValue: UInt) { self.rawValue = rawValue }
    public static let badge = UNAuthorizationOptions(rawValue: 1)
    public static let sound = UNAuthorizationOptions(rawValue: 2)
    public static let alert = UNAuthorizationOptions(rawValue: 4)
    public static let criticalAlert = UNAuthorizationOptions(rawValue: 16)
    public static let provisional = UNAuthorizationOptions(rawValue: 64)
}

public struct UNNotificationPresentationOptions: OptionSet, Sendable {
    public let rawValue: UInt
    public init(rawValue: UInt) { self.rawValue = rawValue }
    public static let badge = UNNotificationPresentationOptions(rawValue: 1)
    public static let sound = UNNotificationPresentationOptions(rawValue: 2)
    public static let list = UNNotificationPresentationOptions(rawValue: 8)
    public static let banner = UNNotificationPresentationOptions(rawValue: 16)
}

public struct UNNotificationActionOptions: OptionSet, Sendable {
    public let rawValue: UInt
    public init(rawValue: UInt) { self.rawValue = rawValue }
    public static let authenticationRequired = UNNotificationActionOptions(rawValue: 1)
    public static let destructive = UNNotificationActionOptions(rawValue: 2)
    public static let foreground = UNNotificationActionOptions(rawValue: 4)
}

public struct UNNotificationCategoryOptions: OptionSet, Sendable {
    public let rawValue: UInt
    public init(rawValue: UInt) { self.rawValue = rawValue }
    public static let customDismissAction = UNNotificationCategoryOptions(rawValue: 1)
    public static let hiddenPreviewsShowTitle = UNNotificationCategoryOptions(rawValue: 4)
    public static let hiddenPreviewsShowSubtitle = UNNotificationCategoryOptions(rawValue: 8)
}

public struct UNNotificationInterruptionLevel: Equatable, Sendable {
    let raw: UInt
    public static let passive = UNNotificationInterruptionLevel(raw: 0)
    public static let active = UNNotificationInterruptionLevel(raw: 1)
    public static let timeSensitive = UNNotificationInterruptionLevel(raw: 2)
    public static let critical = UNNotificationInterruptionLevel(raw: 3)
}

/// Imported `NS_ENUM`s are open: switches over them need `@unknown
/// default`, so the stub models them as structs of constants.
public struct UNAuthorizationStatus: Equatable, Sendable {
    let raw: Int
    public static let notDetermined = UNAuthorizationStatus(raw: 0)
    public static let denied = UNAuthorizationStatus(raw: 1)
    public static let authorized = UNAuthorizationStatus(raw: 2)
    public static let provisional = UNAuthorizationStatus(raw: 3)
    public static let ephemeral = UNAuthorizationStatus(raw: 4)
}

public struct UNNotificationSetting: Equatable, Sendable {
    let raw: Int
    public static let notSupported = UNNotificationSetting(raw: 0)
    public static let disabled = UNNotificationSetting(raw: 1)
    public static let enabled = UNNotificationSetting(raw: 2)
}

open class UNNotificationSettings: NSObject {
    open var authorizationStatus: UNAuthorizationStatus { .notDetermined }
    open var soundSetting: UNNotificationSetting { .disabled }
    open var badgeSetting: UNNotificationSetting { .disabled }
    open var alertSetting: UNNotificationSetting { .disabled }
    open var timeSensitiveSetting: UNNotificationSetting { .disabled }
    open var directMessagesSetting: UNNotificationSetting { .disabled }
}

open class UNNotificationSound: NSObject, @unchecked Sendable {
    public static let `default` = UNNotificationSound()
    public static let defaultCritical = UNNotificationSound()
    public init(named name: UNNotificationSoundName) {}
    override init() {}
}

open class UNNotificationAttachment: NSObject {
    public init(identifier: String, url: URL, options: [AnyHashable: Any]? = nil) throws {}
}

public protocol UNNotificationContentProviding: NSObjectProtocol {}

open class UNNotificationContent: NSObject, @unchecked Sendable {
    open var title: String { "" }
    open var subtitle: String { "" }
    open var body: String { "" }
    open var threadIdentifier: String { "" }
    open var categoryIdentifier: String { "" }
    open var userInfo: [AnyHashable: Any] { [:] }
    open func mutableCopy() -> Any { UNMutableNotificationContent() }
    open func updating(from provider: UNNotificationContentProviding) throws -> UNNotificationContent { self }
}

open class UNMutableNotificationContent: UNNotificationContent, @unchecked Sendable {
    private var storedInfo: [AnyHashable: Any] = [:]
    open override var title: String { get { "" } set {} }
    open override var subtitle: String { get { "" } set {} }
    open override var body: String { get { "" } set {} }
    open override var threadIdentifier: String { get { "" } set {} }
    open override var categoryIdentifier: String { get { "" } set {} }
    open override var userInfo: [AnyHashable: Any] { get { storedInfo } set { storedInfo = newValue } }
    open var sound: UNNotificationSound?
    open var badge: NSNumber?
    open var attachments: [UNNotificationAttachment] = []
    open var targetContentIdentifier: String?
    @available(iOS 15.0, *)
    open var interruptionLevel: UNNotificationInterruptionLevel { get { .active } set {} }
    @available(iOS 15.0, *)
    open var relevanceScore: Double { get { 0 } set {} }
    @available(iOS 16.0, *)
    open var filterCriteria: String? { get { nil } set {} }
}

open class UNNotificationTrigger: NSObject, @unchecked Sendable {
    open var repeats: Bool { false }
}

open class UNTimeIntervalNotificationTrigger: UNNotificationTrigger, @unchecked Sendable {
    public let timeInterval: TimeInterval
    public init(timeInterval: TimeInterval, repeats: Bool) { self.timeInterval = timeInterval }
    /// Like the SDK: the interval from now (the real one counts from the
    /// request's add).
    open func nextTriggerDate() -> Date? { Date(timeIntervalSinceNow: timeInterval) }
}

open class UNCalendarNotificationTrigger: UNNotificationTrigger, @unchecked Sendable {
    public let dateComponents: DateComponents
    public init(dateMatching dateComponents: DateComponents, repeats: Bool) { self.dateComponents = dateComponents }
    /// Not modelled (the stub has no calendar math): the plugin computes
    /// its fire times itself and never reads this.
    open func nextTriggerDate() -> Date? { nil }
}

open class UNNotificationRequest: NSObject, @unchecked Sendable {
    public let identifier: String
    public let content: UNNotificationContent
    public let trigger: UNNotificationTrigger?
    public init(identifier: String, content: UNNotificationContent, trigger: UNNotificationTrigger?) {
        self.identifier = identifier
        self.content = content
        self.trigger = trigger
    }
}

open class UNNotification: NSObject, @unchecked Sendable {
    open var request: UNNotificationRequest {
        UNNotificationRequest(identifier: "", content: UNNotificationContent(), trigger: nil)
    }
    open var date: Date { Date() }
}

open class UNNotificationResponse: NSObject, @unchecked Sendable {
    open var notification: UNNotification { UNNotification() }
    open var actionIdentifier: String { "" }
}

open class UNTextInputNotificationResponse: UNNotificationResponse, @unchecked Sendable {
    open var userText: String { "" }
}

open class UNNotificationAction: NSObject, @unchecked Sendable {
    public init(identifier: String, title: String, options: UNNotificationActionOptions = []) {}
}

open class UNTextInputNotificationAction: UNNotificationAction, @unchecked Sendable {
    public init(identifier: String, title: String, options: UNNotificationActionOptions = [],
                textInputButtonTitle: String, textInputPlaceholder: String) {
        super.init(identifier: identifier, title: title, options: options)
    }
}

open class UNNotificationCategory: NSObject, Hashable, @unchecked Sendable {
    public let identifier: String
    public init(identifier: String, actions: [UNNotificationAction], intentIdentifiers: [String],
                options: UNNotificationCategoryOptions = []) {
        self.identifier = identifier
    }
    public init(identifier: String, actions: [UNNotificationAction], intentIdentifiers: [String],
                hiddenPreviewsBodyPlaceholder: String, options: UNNotificationCategoryOptions = []) {
        self.identifier = identifier
    }
    public static func == (a: UNNotificationCategory, b: UNNotificationCategory) -> Bool { a === b }
    public func hash(into hasher: inout Hasher) { hasher.combine(ObjectIdentifier(self)) }
}

/// The SDK's methods are `@objc optional`; Swift without Objective-C
/// interop models that with default implementations. Signatures as in
/// the current SDK: completion handlers are `@Sendable`.
public protocol UNUserNotificationCenterDelegate: NSObjectProtocol {
    func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping @Sendable (UNNotificationPresentationOptions) -> Void)
    func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping @Sendable () -> Void)
    func userNotificationCenter(_ center: UNUserNotificationCenter, openSettingsFor notification: UNNotification?)
}

public extension UNUserNotificationCenterDelegate {
    func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping @Sendable (UNNotificationPresentationOptions) -> Void) {}
    func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping @Sendable () -> Void) {}
    func userNotificationCenter(_ center: UNUserNotificationCenter, openSettingsFor notification: UNNotification?) {}
}

open class UNUserNotificationCenter: NSObject, @unchecked Sendable {
    public static func current() -> UNUserNotificationCenter { UNUserNotificationCenter() }
    open weak var delegate: UNUserNotificationCenterDelegate?
    open func requestAuthorization(options: UNAuthorizationOptions) async throws -> Bool { false }
    open func setNotificationCategories(_ categories: Set<UNNotificationCategory>) {}
    open func notificationCategories() async -> Set<UNNotificationCategory> { [] }
    open func notificationSettings() async -> UNNotificationSettings { UNNotificationSettings() }
    open func add(_ request: UNNotificationRequest) async throws {}
    open func add(_ request: UNNotificationRequest,
                  withCompletionHandler completionHandler: (@Sendable (Error?) -> Void)?) {}
    open func pendingNotificationRequests() async -> [UNNotificationRequest] { [] }
    open func removePendingNotificationRequests(withIdentifiers identifiers: [String]) {}
    open func removeAllPendingNotificationRequests() {}
    open func deliveredNotifications() async -> [UNNotification] { [] }
    open func removeDeliveredNotifications(withIdentifiers identifiers: [String]) {}
    open func removeAllDeliveredNotifications() {}
}
