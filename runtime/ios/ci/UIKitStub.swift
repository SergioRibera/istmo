// Stand-in for the UIKit slice the screen-capture native backend uses:
// app life-cycle notifications and `NSValue`'s geometry accessor.

import CoreGraphics
import Foundation

open class UIApplication: NSObject {
    public static let didEnterBackgroundNotification = Notification.Name("UIApplicationDidEnterBackgroundNotification")
    public static let willEnterForegroundNotification = Notification.Name("UIApplicationWillEnterForegroundNotification")
}

public extension NSValue {
    var cgRectValue: CGRect { .zero }
}

// Life cycle and settings links, for the notifications native backend.
public extension UIApplication {
    static let didBecomeActiveNotification = Notification.Name("UIApplicationDidBecomeActiveNotification")
    static let openSettingsURLString = "app-settings:"
    @available(iOS 16.0, *)
    static let openNotificationSettingsURLString = "app-settings:notifications"
    struct OpenExternalURLOptionsKey: Hashable, Sendable {
        public let rawValue: String
    }
    static var shared: UIApplication { UIApplication() }
    func open(_ url: URL, options: [OpenExternalURLOptionsKey: Any] = [:],
              completionHandler: ((Bool) -> Void)? = nil) {}
}
