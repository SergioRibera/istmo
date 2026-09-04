// iOS impl of `NotificationsBackend`.
//
// Wraps `UNUserNotificationCenter`:
//
// * `is_authorized` / `request_authorization` mirror Android's shape —
//   query and prompt respectively. iOS's authorisation is process-wide,
//   not per-channel; `channel_id` in the `NotificationRequest` body is
//   ignored on iOS.
// * `schedule` builds a `UNNotificationRequest` and passes it to
//   `add(_:)`. Immediate posts use a nil trigger; delayed posts use a
//   `UNTimeIntervalNotificationTrigger`.
// * `cancel(id)` maps to `removePendingNotificationRequests` +
//   `removeDeliveredNotifications` on the string form of the id (matches
//   the ids we hand out on `schedule`).
//
// `NotificationHandle.id` is drawn from a monotonic counter; the request
// identifier the OS stores is the decimal string form of that id, so
// `cancel` can round-trip.

import Foundation
import IstmoRuntime
import UserNotifications

public final class NotificationsBackendImpl: NotificationsBackend {

    private let center: UNUserNotificationCenter
    private let idLock = NSLock()
    private var _nextId: UInt32 = 1

    public init(center: UNUserNotificationCenter = .current()) {
        self.center = center
    }

    public func is_authorized() async throws -> Bool {
        let settings = await center.notificationSettings()
        switch settings.authorizationStatus {
        case .authorized, .provisional, .ephemeral: return true
        default: return false
        }
    }

    public func request_authorization() async throws -> Bool {
        do {
            let granted = try await center.requestAuthorization(options: [.alert, .badge, .sound])
            return granted
        } catch {
            NSLog("NotificationsBackendImpl: requestAuthorization threw: \(error)")
            return false
        }
    }

    public func schedule(request: NotificationRequest) async throws -> NotificationHandle {
        // Bail early if the user has denied — otherwise `add` accepts the
        // request but the notification never fires, which is worse UX
        // than a domain error the caller can display.
        let settings = await center.notificationSettings()
        switch settings.authorizationStatus {
        case .authorized, .provisional, .ephemeral: break
        default: throw NotificationError.permissionDenied
        }

        idLock.lock()
        let id = _nextId
        _nextId += 1
        idLock.unlock()

        let content = UNMutableNotificationContent()
        content.title = request.title
        content.body = request.body
        content.sound = mapSound(request.importance)
        // `interruptionLevel` landed in iOS 15. On earlier OS the property
        // is not visible so we silently fall through — the notification
        // uses the default (active) presentation.
        if #available(iOS 15.0, *) {
            content.interruptionLevel = mapInterruption(request.importance)
        }

        let trigger: UNNotificationTrigger?
        if let delay = request.delaySeconds, delay > 0 {
            trigger = UNTimeIntervalNotificationTrigger(
                timeInterval: TimeInterval(delay),
                repeats: false,
            )
        } else {
            trigger = nil
        }

        let identifier = request.tag ?? String(id)
        let osRequest = UNNotificationRequest(
            identifier: identifier,
            content: content,
            trigger: trigger,
        )
        do {
            try await center.add(osRequest)
        } catch {
            throw NotificationError.scheduler("\(error)")
        }
        return NotificationHandle(id: id)
    }

    public func cancel(id: UInt32) async throws {
        let identifier = String(id)
        center.removePendingNotificationRequests(withIdentifiers: [identifier])
        center.removeDeliveredNotifications(withIdentifiers: [identifier])
    }

    // MARK: - Importance mapping

    private func mapSound(_ importance: NotificationImportance) -> UNNotificationSound? {
        switch importance {
        case .min, .low: return nil
        case .default, .high: return .default
        }
    }

    @available(iOS 15.0, *)
    private func mapInterruption(_ importance: NotificationImportance) -> UNNotificationInterruptionLevel {
        switch importance {
        case .min: return .passive
        case .low: return .passive
        case .default: return .active
        case .high: return .timeSensitive
        }
    }
}
