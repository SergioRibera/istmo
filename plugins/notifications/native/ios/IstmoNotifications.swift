import Foundation
import IstmoRuntime
#if canImport(UIKit)
import UIKit
#endif
import UserNotifications

/// App side of the notifications plugin on iOS.
///
/// The generated plugin registry builds `NotificationsBackendImpl`
/// before `UIApplicationMain`, and its initializer calls `install()`:
/// the `UNUserNotificationCenter` delegate is in place before launch
/// finishes, so the tap that started the app reaches Rust (buffered on
/// the `istmo.notifications.responses` queue until it subscribes).
///
/// Apps that set their own `UNUserNotificationCenter` delegate forward
/// both callbacks; each returns `false` for notifications this plugin did
/// not post, which the app then handles itself:
///
/// ```swift
/// func userNotificationCenter(_ center: UNUserNotificationCenter,
///                             willPresent notification: UNNotification,
///                             withCompletionHandler done: @escaping (UNNotificationPresentationOptions) -> Void) {
///     if !IstmoNotifications.willPresent(notification, completion: done) { done([.banner, .list]) }
/// }
/// func userNotificationCenter(_ center: UNUserNotificationCenter,
///                             didReceive response: UNNotificationResponse,
///                             withCompletionHandler done: @escaping () -> Void) {
///     if !IstmoNotifications.didReceive(response, completion: done) { done() }
/// }
/// ```
public enum IstmoNotifications {
    /// Sets the plugin's delegate (unless the app set one already) and
    /// starts observing the app's life cycle. Idempotent; call it before
    /// launch finishes when the plugin is registered by hand.
    public static func install() {
        NotificationsCore.shared.install()
    }

    /// Foreground presentation of a notification. `false` when it is not
    /// one of this plugin's (the completion handler is then not called).
    @discardableResult
    public static func willPresent(
        _ notification: UNNotification,
        completion: @escaping (UNNotificationPresentationOptions) -> Void
    ) -> Bool {
        NotificationsCore.shared.willPresent(notification, completion: completion)
    }

    /// A tap, a button or a reply. `false` when the notification is not
    /// one of this plugin's (the completion handler is then not called).
    /// For background actions the handler is held until the app calls
    /// `complete(ack)` from Rust, or for at most 25 seconds.
    @discardableResult
    public static func didReceive(
        _ response: UNNotificationResponse,
        completion: @escaping () -> Void
    ) -> Bool {
        NotificationsCore.shared.didReceive(response, completion: completion)
    }
}

/// The delegate `install()` sets.
final class IstmoNotificationsDelegate: NSObject, UNUserNotificationCenterDelegate {
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        if !IstmoNotifications.willPresent(notification, completion: completionHandler) {
            completionHandler([.banner, .list, .sound])
        }
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        if !IstmoNotifications.didReceive(response, completion: completionHandler) {
            completionHandler()
        }
    }
}
