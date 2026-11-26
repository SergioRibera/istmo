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
