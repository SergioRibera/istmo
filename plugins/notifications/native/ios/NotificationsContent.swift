import Foundation
#if canImport(Intents)
import Intents
#endif
import IstmoRuntime
import UserNotifications

/// A built content and the attachment copies it points at: the system
/// moves those into its store when the request is added; when the add
/// fails they are the caller's to delete.
struct NotificationsPrepared {
    let content: UNNotificationContent
    let files: [URL]

    func discardFiles() {
        for file in files { try? FileManager.default.removeItem(at: file) }
    }
}

/// Builds the `UNNotificationContent` of one notification.
extension NotificationsCore {
    /// Render limits on iOS for `n`.
    func limits(for n: Notifications.Notification) -> NotificationsRender.Limits {
        var messaging = false
        if case .messaging(let c) = n.style, communication, c.messages.last?.sender != nil { messaging = true }
        return NotificationsRender.Limits(maxActions: Self.maxActions, messaging: messaging)
    }

    /// `quiet` = a silent update: no sound, passive, list only. `donate`
    /// = donate the conversation's `INInteraction` (an alerting post shown
    /// now); silent updates and occurrences armed ahead of time build the
    /// communication content without donating again.
    func content(
        for n: Notifications.Notification, rendered: NotificationsRender.Rendered, quiet: Bool, donate: Bool
    ) async -> NotificationsPrepared {
        let messaging = limits(for: n).messaging
        let content = UNMutableNotificationContent()
        content.title = n.title
        if let subtitle = n.subtitle { content.subtitle = subtitle }
        content.body = rendered.body ?? ""
        if let group = n.group {
            content.threadIdentifier = group.id.value
        } else if case .messaging = n.style {
            content.threadIdentifier = n.id.value
        }
        content.categoryIdentifier = await category(for: rendered, lockScreen: n.lockScreen,
                                                    communication: messaging)
        content.sound = quiet ? nil : Self.sound(n)
        if #available(iOS 15.0, *) {
            content.interruptionLevel = quiet ? .passive : Self.level(n.interruption)
            if let score = n.apple?.relevanceScore { content.relevanceScore = Double(min(max(score, 0), 1)) }
        }
        if #available(iOS 16.0, *), let criteria = n.apple?.filterCriteria { content.filterCriteria = criteria }
        if let target = n.apple?.targetContentId { content.targetContentIdentifier = target }
        let (attachments, files) = Self.attachments(n)
        content.attachments = attachments
        var info: [AnyHashable: Any] = [
            NotificationsKeys.id: n.id.value,
            NotificationsKeys.keys: NotificationsKeyMap(rendered, codecs: codecs).encode(),
            NotificationsKeys.present: ForegroundBits.of(n, quiet: quiet),
        ]
        if let userData = n.userData { info[NotificationsKeys.userData] = userData }
        if let url = n.url { info[NotificationsKeys.url] = url }
        content.userInfo = info
        // Badge untouched: the app badge belongs to istmo-launcher-actions.
        if messaging, #available(iOS 15.0, *), case .messaging(let conversation) = n.style {
            #if canImport(Intents)
            if let updated = await NotificationsCommunication.update(content, n, conversation, donate: donate) {
                return NotificationsPrepared(content: updated, files: files)
            }
            #endif
            // Donation refused: the conversation as body lines.
            var plain = limits(for: n)
            plain.messaging = false
            content.body = NotificationsRender.body(n, plain) ?? ""
        }
        return NotificationsPrepared(content: content, files: files)
    }

    private static func sound(_ n: Notifications.Notification) -> UNNotificationSound? {
        if n.interruption == .critical, NotificationsCore.config["Critical"] as? Bool == true {
            return .defaultCritical
        }
        switch n.sound {
        case nil, .default?: return .default
        case .silent?: return nil
        case .named(let name)?: return namedSound(name)
        }
    }

    /// `[app.notifications] sounds`: `IstmoNotifications.Sounds[name]` is the
    /// file istmo-build copied to the bundle root; else a bundled
    /// `<name>.<ext>`, else the name as given (a system or library sound).
    private static func namedSound(_ name: String) -> UNNotificationSound {
        if let sounds = NotificationsCore.config["Sounds"] as? [String: String], let file = sounds[name] {
            return UNNotificationSound(named: UNNotificationSoundName(rawValue: file))
        }
        for ext in ["caf", "wav", "aiff", "aif"] where Bundle.main.url(forResource: name, withExtension: ext) != nil {
            return UNNotificationSound(named: UNNotificationSoundName(rawValue: "\(name).\(ext)"))
        }
        return UNNotificationSound(named: UNNotificationSoundName(rawValue: name))
    }

    @available(iOS 15.0, *)
    private static func level(_ i: Notifications.Interruption?) -> UNNotificationInterruptionLevel {
        let timeSensitive = NotificationsCore.config["TimeSensitive"] as? Bool == true
        switch i {
        case .passive?: return .passive
        // Each level needs its entitlement (`IstmoNotifications` flags);
        // without it the level falls back one step.
        case .urgent?: return timeSensitive ? .timeSensitive : .active
        case .critical?:
            if NotificationsCore.config["Critical"] as? Bool == true { return .critical }
            return timeSensitive ? .timeSensitive : .active
        default: return .active
        }
    }

    /// The picture, else the icon, as a file attachment, and the copy it
    /// uses. The system moves attachment files into its store, so each
    /// post writes a fresh copy; a refused one is deleted at once.
    private static func attachments(_ n: Notifications.Notification) -> ([UNNotificationAttachment], [URL]) {
        var image: Notifications.NotificationImage? = n.icon
        if case .picture(let picture) = n.style { image = picture }
        guard let image = image, let url = attachmentFile(image) else { return ([], []) }
        do {
            return ([try UNNotificationAttachment(identifier: "istmo.image", url: url, options: nil)], [url])
        } catch {
            NSLog("istmo-notifications: attachment refused: \(error)")
            try? FileManager.default.removeItem(at: url)
            return ([], [])
        }
    }

    /// File extension of an image from its first bytes (PNG, JPEG, GIF):
    /// the system types an attachment by its extension. Rust's
    /// `apple_map::image_extension`.
    static func imageExtension(_ data: Data) -> String? {
        let head = Array(data.prefix(8))
        if head.starts(with: [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) { return "png" }
        if head.starts(with: [0xff, 0xd8, 0xff]) { return "jpg" }
        if head.starts(with: Array("GIF87a".utf8)) || head.starts(with: Array("GIF89a".utf8)) { return "gif" }
        return nil
    }

    static func attachmentFile(_ image: Notifications.NotificationImage) -> URL? {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("istmo-notifications", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true, attributes: nil)
        let name = UUID().uuidString
        do {
            switch image {
            case .png(let data):
                let url = dir.appendingPathComponent("\(name).png")
                try data.write(to: url)
                return url
            case .jpeg(let data):
                let url = dir.appendingPathComponent("\(name).jpg")
                try data.write(to: url)
                return url
            case .file(let path):
                let source = URL(fileURLWithPath: path)
                var ext = source.pathExtension
                if ext.isEmpty {
                    // No extension: the type from the content, else PNG.
                    ext = (try? Data(contentsOf: source)).flatMap(imageExtension) ?? "png"
                }
                let url = dir.appendingPathComponent("\(name).\(ext)")
                try FileManager.default.copyItem(at: source, to: url)
                return url
            case .named:
                // Asset catalog images cannot be attachments.
                return nil
            }
        } catch {
            NSLog("istmo-notifications: image not written: \(error)")
            return nil
        }
    }
}

#if canImport(Intents)
/// Communication notifications (iOS 15+): an `INSendMessageIntent` with
/// the sender as an `INPerson`, donated as an incoming interaction, then
/// `content.updating(from:)`. Needs the
/// `com.apple.developer.usernotifications.communication` entitlement and
/// `INSendMessageIntent` in `NSUserActivityTypes`.
@available(iOS 15.0, *)
enum NotificationsCommunication {
    /// `donate` = false: the content is built from the intent without
    /// donating the interaction again (a silent update, an occurrence
    /// armed before its message arrives).
    static func update(
        _ content: UNMutableNotificationContent, _ n: Notifications.Notification,
        _ conversation: Notifications.Conversation, donate: Bool
    ) async -> UNNotificationContent? {
        guard let last = conversation.messages.last, let sender = last.sender else { return nil }
        let me = person(conversation.user, isMe: true)
        var recipients = [me]
        if conversation.isGroup {
            var seen: Set<String> = [sender.key, conversation.user.key]
            for message in conversation.messages {
                if let other = message.sender, seen.insert(other.key).inserted {
                    recipients.append(person(other, isMe: false))
                }
            }
        }
        let groupName = conversation.isGroup ? conversation.title.map { INSpeakableString(spokenPhrase: $0) } : nil
        let intent = INSendMessageIntent(
            recipients: recipients, outgoingMessageType: .outgoingMessageText, content: last.text,
            speakableGroupName: groupName, conversationIdentifier: n.id.value, serviceName: nil,
            sender: person(sender, isMe: false), attachments: nil)
        do {
            if donate {
                let interaction = INInteraction(intent: intent, response: nil)
                interaction.direction = .incoming
                try await interaction.donate()
            }
            return try content.updating(from: intent)
        } catch {
            NSLog("istmo-notifications: communication notification refused: \(error)")
            return nil
        }
    }

    private static func person(_ p: Notifications.Participant, isMe: Bool) -> INPerson {
        INPerson(
            personHandle: INPersonHandle(value: p.key, type: .unknown), nameComponents: nil,
            displayName: p.name, image: p.avatar.flatMap(image), contactIdentifier: nil,
            customIdentifier: p.key, isMe: isMe, suggestionType: .none)
    }

    private static func image(_ image: Notifications.NotificationImage) -> INImage? {
        switch image {
        case .png(let data), .jpeg(let data): return INImage(imageData: data)
        // The bytes, not the URL: an `INImage(url:)` is read lazily, by
        // another process that cannot open the app's files.
        case .file(let path): return (try? Data(contentsOf: URL(fileURLWithPath: path))).map { INImage(imageData: $0) }
        case .named(let name): return INImage(named: name)
        }
    }
}
#endif
