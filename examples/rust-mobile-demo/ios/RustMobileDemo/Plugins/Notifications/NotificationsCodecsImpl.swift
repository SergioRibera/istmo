// Codec impl for the `Notifications` plugin's `Named` types.
//
// Wire encoding mirrors the Rust `#[message]` derive output:
//
// * `NotificationImportance` — u32 varint discriminant.
// * `NotificationRequest` — struct: `String` title, `String` body,
//   `String` channel_id, `NotificationImportance`, `Option<u32>` delay
//   (0/1 tag + optional u32 varint), `Option<String>` tag.
// * `NotificationHandle` — single `u32` varint (single-field struct is
//   encoded as the field itself under bincode 2's default `standard()`
//   config).
// * `NotificationError` — u32 varint discriminant, then per-variant
//   payload (`String` for `InvalidChannel` and `Scheduler`).

import Foundation
import IstmoRuntime

public final class NotificationsCodecsImpl: NotificationsCodecs {

    public init() {}

    public func readNotificationRequest(_ c: inout Bincode.Cursor) throws -> NotificationRequest {
        let title = try Bincode.readString(&c)
        let body = try Bincode.readString(&c)
        let channelId = try Bincode.readString(&c)
        let importance = try readNotificationImportance(&c)
        let delaySeconds = try Bincode.readOption(&c) { try Bincode.readVarintU32(&$0) }
        let tag = try Bincode.readOption(&c) { try Bincode.readString(&$0) }
        return NotificationRequest(
            title: title,
            body: body,
            channelId: channelId,
            importance: importance,
            delaySeconds: delaySeconds,
            tag: tag,
        )
    }

    public func writeNotificationRequest(_ out: inout Data, _ value: NotificationRequest) {
        Bincode.writeString(&out, value.title)
        Bincode.writeString(&out, value.body)
        Bincode.writeString(&out, value.channelId)
        writeNotificationImportance(&out, value.importance)
        Bincode.writeOption(&out, value.delaySeconds) { Bincode.writeVarintU32(&$0, $1) }
        Bincode.writeOption(&out, value.tag) { Bincode.writeString(&$0, $1) }
    }

    public func readNotificationHandle(_ c: inout Bincode.Cursor) throws -> NotificationHandle {
        NotificationHandle(id: try Bincode.readVarintU32(&c))
    }

    public func writeNotificationHandle(_ out: inout Data, _ value: NotificationHandle) {
        Bincode.writeVarintU32(&out, value.id)
    }

    public func readNotificationError(_ c: inout Bincode.Cursor) throws -> NotificationError {
        let disc = try Bincode.readVarintU32(&c)
        switch disc {
        case 0:
            return .permissionDenied
        case 1:
            return .invalidChannel(try Bincode.readString(&c))
        case 2:
            return .scheduler(try Bincode.readString(&c))
        default:
            throw Bincode.DecodeError.invalidTag(UInt8(clamping: disc))
        }
    }

    public func writeNotificationError(_ out: inout Data, _ value: NotificationError) {
        switch value {
        case .permissionDenied:
            Bincode.writeVarintU32(&out, 0)
        case .invalidChannel(let s):
            Bincode.writeVarintU32(&out, 1)
            Bincode.writeString(&out, s)
        case .scheduler(let s):
            Bincode.writeVarintU32(&out, 2)
            Bincode.writeString(&out, s)
        }
    }

    // MARK: - NotificationImportance (not in the Codecs protocol — only
    // referenced through `NotificationRequest`)

    private func readNotificationImportance(_ c: inout Bincode.Cursor) throws -> NotificationImportance {
        let disc = try Bincode.readVarintU32(&c)
        guard let value = NotificationImportance(rawValue: disc) else {
            throw Bincode.DecodeError.invalidTag(UInt8(clamping: disc))
        }
        return value
    }

    private func writeNotificationImportance(_ out: inout Data, _ value: NotificationImportance) {
        Bincode.writeVarintU32(&out, value.rawValue)
    }
}
