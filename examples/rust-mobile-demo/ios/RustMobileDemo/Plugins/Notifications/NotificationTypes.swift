// Swift mirrors of the Rust `#[message]` types in
// `istmo-plugins/src/notifications.rs`.
//
// Ordering of `NotificationImportance` and `NotificationError` variants
// must match the Rust source — bincode encodes enum discriminants as u32
// varints in declaration order.

import Foundation

public enum NotificationImportance: UInt32 {
    case min = 0
    case low = 1
    case `default` = 2
    case high = 3
}

public struct NotificationRequest {
    public let title: String
    public let body: String
    public let channelId: String
    public let importance: NotificationImportance
    public let delaySeconds: UInt32?
    public let tag: String?

    public init(
        title: String,
        body: String,
        channelId: String,
        importance: NotificationImportance,
        delaySeconds: UInt32?,
        tag: String?
    ) {
        self.title = title
        self.body = body
        self.channelId = channelId
        self.importance = importance
        self.delaySeconds = delaySeconds
        self.tag = tag
    }
}

/// Handle to a scheduled notification. `id` matches the Android
/// `notificationId` / iOS request identifier hash.
public struct NotificationHandle {
    public let id: UInt32
    public init(id: UInt32) { self.id = id }
}

/// Domain-error type. Backends throw a case; the dispatcher encodes it
/// via `NotificationsCodecsImpl.writeNotificationError` and the Rust
/// caller sees `IstmoError::PluginError { bytes }` decodable to
/// `istmo_plugins::NotificationError`.
public enum NotificationError: Error {
    case permissionDenied
    case invalidChannel(String)
    case scheduler(String)
}
