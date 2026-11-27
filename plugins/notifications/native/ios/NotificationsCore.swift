import Foundation
import IstmoRuntime
#if canImport(UIKit)
import UIKit
#endif
import UserNotifications

/// `userInfo` keys of the plugin's notifications.
enum NotificationsKeys {
    /// The notification id (request ids of calendar rules differ).
    static let id = "istmo.id"
    static let userData = "istmo.ud"
    static let url = "istmo.url"
    /// `NotificationsKeyMap` bytes: what each action key means.
    static let keys = "istmo.k"
    /// Foreground presentation bits (`ForegroundBits`).
    static let present = "istmo.p"
    /// A snoozed copy (left alone by the schedule top-up).
    static let snoozed = "istmo.s"
}

/// Foreground presentation, as bits in `userInfo`.
enum ForegroundBits {
    static let banner = 1
    static let list = 2
    static let sound = 4
    static let badge = 8

    static func of(_ n: Notifications.Notification, quiet: Bool) -> Int {
        if n.hideInForeground { return 0 }
        // A silent update only refreshes the list entry.
        if quiet { return list }
        guard let fg = n.apple?.foreground else { return banner | list | sound }
        return (fg.banner ? banner : 0) | (fg.list ? list : 0) | (fg.sound ? sound : 0) | (fg.badge ? badge : 0)
    }

    static func options(_ bits: Int) -> UNNotificationPresentationOptions {
        var out: UNNotificationPresentationOptions = []
        if bits & banner != 0 { out.insert(.banner) }
        if bits & list != 0 { out.insert(.list) }
        if bits & sound != 0 { out.insert(.sound) }
        if bits & badge != 0 { out.insert(.badge) }
        return out
    }
}

/// Lanes on the Rust side.
enum NotificationsLanes {
    /// `istmo_notifications::RESPONSES_CHANNEL` / `RESPONSES_CAPACITY`.
    static let responses = "istmo.notifications.responses"
    static let responsesCapacity: UInt32 = 64
    /// `istmo_plugins::DEEPLINKS_CHANNEL` / `DEFAULT_DEEPLINKS_CAPACITY`.
    static let deepLinks = "istmo.deeplinks"
    static let deepLinksCapacity: UInt32 = 16

    /// `istmo_plugins::DeepLink { uri, source, received_at_ms }` in its
    /// wire layout.
    static func deepLink(uri: String, source: String, receivedAtMs: UInt64) -> Data {
        var out = Data()
        Bincode.writeString(&out, uri)
        Bincode.writeU8(&out, 1)
        Bincode.writeString(&out, source)
        Bincode.writeU8(&out, 1)
        Bincode.writeVarintU64(&out, receivedAtMs)
        return out
    }
}

extension NSLock {
    /// `withLock` for iOS 14 (`NSLocking.withLock` is iOS 16+).
    func guarded<R>(_ body: () throws -> R) rethrows -> R {
        lock()
        defer { unlock() }
        return try body()
    }
}

func nowMs() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }

/// Process-wide state of the iOS backend: the delegate's side (responses,
/// acks, events) and what the backend shows (live table, coalescer,
/// categories, schedule store). Every field is guarded by `lock`, never
/// held across an `await`.
final class NotificationsCore: @unchecked Sendable {
    static let shared = NotificationsCore()

    /// iOS shows at most four actions.
    static let maxActions = 4
    /// Background actions keep the process this long without `complete`.
    static let ackTimeout: TimeInterval = 25
    /// The event backlog kept for the first subscriber.
    static let backlog = 64

    let center = UNUserNotificationCenter.current()
    let codecs = NotificationsCodecsImpl()
    let lock = NSLock()
    private let delegate = IstmoNotificationsDelegate()
    private var installed = false
    /// No `didBecomeActive` seen yet: a response now launched the app.
    private var becameActive = false
    private var observers: [NSObjectProtocol] = []

    // Shown notifications (latest content, queued updates included).
    var live: [String: Notifications.Notification] = [:]
    var channels: [String: Notifications.ChannelSpec] = [:]
    var coalescer = NotificationsCoalescer<Notifications.Notification>(rate: .apple, nowMs: nowMs())
    var flushArmed = false

    // Categories: replace-all on Apple, so the whole set is kept here.
    private var categories: [String: UNNotificationCategory] = [:]
    private var istmoCategories: [String] = []
    static let maxCategories = 128

    // Background responses waiting for `complete(ack)`.
    private var acks: [UInt64: () -> Void] = [:]
    private var nextAck: UInt64 = 1

    // Events.
    private var backlog: [Notifications.NotificationEvent] = []
    private var subscribers: [UInt64: AsyncThrowingStream<Notifications.NotificationEvent, Error>.Continuation] = [:]
    private var subscribed = false
    private var nextSubscriber: UInt64 = 0

    var store: NotificationsScheduleStore { NotificationsScheduleStore.shared }

    /// `[app.notifications] apple.communication`: `NSUserActivityTypes`
    /// lists `INSendMessageIntent` (iOS 15+).
    let communication: Bool = {
        guard #available(iOS 15.0, *) else { return false }
        let types = Bundle.main.object(forInfoDictionaryKey: "NSUserActivityTypes") as? [String]
        return types?.contains("INSendMessageIntent") ?? false
    }()

    func install() {
        let first: Bool = lock.guarded {
            if installed { return false }
            installed = true
            return true
        }
        guard first else { return }
        if center.delegate == nil {
            center.delegate = delegate
        }
        #if canImport(UIKit)
        let active = NotificationCenter.default.addObserver(
            forName: UIApplication.didBecomeActiveNotification, object: nil, queue: nil
        ) { _ in
            NotificationsCore.shared.lock.guarded { NotificationsCore.shared.becameActive = true }
        }
        let foreground = NotificationCenter.default.addObserver(
            forName: UIApplication.willEnterForegroundNotification, object: nil, queue: nil
        ) { _ in
            Task { await NotificationsCore.shared.store.topUp() }
        }
        lock.guarded { observers = [active, foreground] }
        #endif
        loadStaticChannels()
        Task {
            let existing = await center.notificationCategories()
            lock.guarded {
                for category in existing where categories[category.identifier] == nil {
                    categories[category.identifier] = category
                    if category.identifier.hasPrefix("istmo.") { istmoCategories.append(category.identifier) }
                }
            }
            await store.topUp()
        }
    }

    /// `[[app.notification_channel]]`: wire bytes of `Vec<ChannelSpec>` in
    /// the bundle as `istmo_notification_channels.bin` (written by
    /// istmo-build); absent = no static channels.
    private func loadStaticChannels() {
        guard let url = Bundle.main.url(forResource: "istmo_notification_channels", withExtension: "bin"),
              let data = try? Data(contentsOf: url) else { return }
        var c = Bincode.Cursor(data)
        guard let specs = try? Bincode.readVec(&c, { c1 in try codecs.readChannelSpec(&c1) }) else {
            NSLog("istmo-notifications: unreadable istmo_notification_channels.bin")
            return
        }
        lock.guarded { for spec in specs { channels[spec.id.value] = spec } }
    }

    // MARK: events

    func emit(_ event: Notifications.NotificationEvent) {
        lock.guarded {
            if !subscribed {
                if backlog.count == Self.backlog { backlog.removeFirst() }
                backlog.append(event)
                return
            }
            for (_, cont) in subscribers { cont.yield(event) }
        }
    }

    func events() -> AsyncThrowingStream<Notifications.NotificationEvent, Error> {
        AsyncThrowingStream(bufferingPolicy: .bufferingNewest(256)) { cont in
            let key: UInt64 = lock.guarded {
                nextSubscriber += 1
                if !subscribed {
                    subscribed = true
                    for event in backlog { cont.yield(event) }
                    backlog.removeAll()
                }
                subscribers[nextSubscriber] = cont
                return nextSubscriber
            }
            cont.onTermination = { _ in
                NotificationsCore.shared.lock.guarded { _ = NotificationsCore.shared.subscribers.removeValue(forKey: key) }
            }
        }
    }

    // MARK: delegate

    func willPresent(
        _ notification: UNNotification,
        completion: @escaping (UNNotificationPresentationOptions) -> Void
    ) -> Bool {
        let info = notification.request.content.userInfo
        guard let id = info[NotificationsKeys.id] as? String else { return false }
        let bits = (info[NotificationsKeys.present] as? Int) ?? (ForegroundBits.banner | ForegroundBits.list)
        completion(ForegroundBits.options(bits))
        // A scheduled one firing while the app is open.
        if notification.request.trigger != nil {
            emit(.delivered(Notifications.NotificationId(value: id)))
            Task { await store.topUp() }
        }
        return true
    }

    func didReceive(_ response: UNNotificationResponse, completion: @escaping () -> Void) -> Bool {
        let request = response.notification.request
        let info = request.content.userInfo
        guard let rawId = info[NotificationsKeys.id] as? String else { return false }
        let id = Notifications.NotificationId(value: rawId)
        let launched: Bool = lock.guarded { !becameActive }
        Task { await store.topUp() }

        if response.actionIdentifier == UNNotificationDismissActionIdentifier {
            lock.guarded { coalescer.dismissed(rawId) }
            emit(.dismissed(Notifications.Dismissal(id: id, reason: .user)))
            completion()
            return true
        }

        let keyMap = (info[NotificationsKeys.keys] as? Data).flatMap(NotificationsKeyMap.decode)
        var action: Notifications.ResponseAction = .open
        var reply: String? = (response as? UNTextInputNotificationResponse)?.userText
        var url = info[NotificationsKeys.url] as? String
        var requested = false
        var background = false
        var overflow: [Notifications.Action] = []

        if response.actionIdentifier != UNNotificationDefaultActionIdentifier {
            let key = response.actionIdentifier
            guard let entry = keyMap?.entry(key) else {
                NSLog("istmo-notifications: unknown action \(key)")
                completion()
                return true
            }
            if let ms = entry.snoozeMs {
                snooze(request, afterMs: ms, completion: completion)
                return true
            }
            if key == NotificationsRender.moreKey {
                action = .more
                if let data = keyMap?.overflow {
                    var c = Bincode.Cursor(data)
                    overflow = (try? Bincode.readVec(&c, { c1 in try codecs.readAction(&c1) })) ?? []
                }
            } else if let actionId = entry.actionId {
                action = .button(Notifications.ActionId(value: actionId))
                if let choice = entry.choice { reply = choice }
                if let actionUrl = entry.url { url = actionUrl }
            }
            requested = entry.inputRequested
            background = entry.background
        }

        var ack: Notifications.AckToken?
        if background {
            ack = lock.guarded { () -> Notifications.AckToken in
                let value = nextAck
                nextAck += 1
                acks[value] = completion
                return Notifications.AckToken(value: value)
            }
            if let value = ack?.value {
                DispatchQueue.global().asyncAfter(deadline: .now() + Self.ackTimeout) {
                    NotificationsCore.shared.finish(ack: value)
                }
            }
        }
        let out = Notifications.NotificationResponse(
            id: id, action: action, reply: reply, inputRequested: requested,
            userData: info[NotificationsKeys.userData] as? String, url: url, launchedApp: launched,
            overflowActions: overflow, ack: ack)
        publish(out)
        if !background { completion() }
        return true
    }

    /// Publishes a response, and its URL as a deep link for taps and
    /// buttons (never for dismissals or "More").
    private func publish(_ response: Notifications.NotificationResponse) {
        var bytes = Data()
        codecs.writeNotificationResponse(&bytes, response)
        IstmoRuntime.shared.publishEarlyQueue(
            channel: NotificationsLanes.responses, capacity: NotificationsLanes.responsesCapacity, payload: bytes)
        guard let url = response.url, !url.isEmpty else { return }
        var source = "notification:\(response.id.value)"
        switch response.action {
        case .open: break
        case .button(let action): source += ":\(action.value)"
        default: return
        }
        IstmoRuntime.shared.publishEarlyQueue(
            channel: NotificationsLanes.deepLinks, capacity: NotificationsLanes.deepLinksCapacity,
            payload: NotificationsLanes.deepLink(uri: url, source: source, receivedAtMs: UInt64(max(nowMs(), 0))))
    }

    /// Shows the same content again after `ms`, without the app's code.
    private func snooze(_ request: UNNotificationRequest, afterMs ms: UInt64, completion: @escaping () -> Void) {
        let content = request.content.mutableCopy() as? UNMutableNotificationContent
        let rawId = request.content.userInfo[NotificationsKeys.id] as? String ?? request.identifier
        center.removeDeliveredNotifications(withIdentifiers: [request.identifier])
        guard let content = content else {
            completion()
            return
        }
        var info = content.userInfo
        info[NotificationsKeys.snoozed] = 1
        content.userInfo = info
        let trigger = UNTimeIntervalNotificationTrigger(timeInterval: max(Double(ms) / 1000, 1), repeats: false)
        center.add(UNNotificationRequest(identifier: request.identifier, content: content, trigger: trigger)) { error in
            if let error = error {
                NotificationsCore.shared.emit(.failed(Notifications.Failure(
                    id: Notifications.NotificationId(value: rawId), message: error.localizedDescription)))
            } else {
                NotificationsCore.shared.emit(.snoozed(Notifications.NotificationId(value: rawId)))
            }
            completion()
        }
    }

    /// Releases a background response. `false` for an unknown token.
    @discardableResult
    func finish(ack: UInt64) -> Bool {
        let done: (() -> Void)? = lock.guarded { acks.removeValue(forKey: ack) }
        done?()
        return done != nil
    }

    // MARK: categories

    /// Registers the category `buttons` need (every notification gets one,
    /// for the custom dismiss action) and returns its identifier.
    func category(for rendered: NotificationsRender.Rendered, lockScreen: Notifications.LockScreen?,
                  communication: Bool) -> String {
        var descriptor = "\(lockScreen.map { String($0.rawValue) } ?? "-")|\(communication)"
        for b in rendered.buttons {
            descriptor += "|\(b.key)\u{1f}\(b.title)\u{1f}\(b.opensApp)\(b.destructive)\(b.requiresUnlock)"
            if case let .reply(placeholder, submit) = b.input {
                descriptor += "\u{1f}r\(placeholder ?? "")\u{1f}\(submit ?? "")"
            }
        }
        let identifier = "istmo.c.\(fnv1a64Hex(descriptor))"
        let set: Set<UNNotificationCategory>? = lock.guarded {
            if categories[identifier] != nil { return nil }
            categories[identifier] = Self.makeCategory(identifier, rendered, lockScreen, communication)
            istmoCategories.append(identifier)
            while istmoCategories.count > Self.maxCategories {
                categories.removeValue(forKey: istmoCategories.removeFirst())
            }
            return Set(categories.values)
        }
        if let set = set { center.setNotificationCategories(set) }
        return identifier
    }

    private static func makeCategory(
        _ identifier: String, _ rendered: NotificationsRender.Rendered,
        _ lockScreen: Notifications.LockScreen?, _ communication: Bool
    ) -> UNNotificationCategory {
        let actions: [UNNotificationAction] = rendered.buttons.map { b in
            var options: UNNotificationActionOptions = []
            if b.opensApp { options.insert(.foreground) }
            if b.destructive { options.insert(.destructive) }
            if b.requiresUnlock { options.insert(.authenticationRequired) }
            if case let .reply(placeholder, submit) = b.input {
                return UNTextInputNotificationAction(
                    identifier: b.key, title: b.title, options: options,
                    textInputButtonTitle: submit ?? "Send", textInputPlaceholder: placeholder ?? "")
            }
            return UNNotificationAction(identifier: b.key, title: b.title, options: options)
        }
        var options: UNNotificationCategoryOptions = [.customDismissAction]
        let intents = communication ? ["INSendMessageIntent"] : []
        switch lockScreen {
        case .public?:
            options.insert(.hiddenPreviewsShowTitle)
            options.insert(.hiddenPreviewsShowSubtitle)
        case .hideContent?, .hidden?:
            return UNNotificationCategory(identifier: identifier, actions: actions, intentIdentifiers: intents,
                                          hiddenPreviewsBodyPlaceholder: "", options: options)
        case nil:
            break
        }
        return UNNotificationCategory(identifier: identifier, actions: actions, intentIdentifiers: intents,
                                      options: options)
    }
}

/// FNV-1a 64, lowercase hex (stable across launches, unlike `hashValue`).
func fnv1a64Hex(_ text: String) -> String {
    var hash: UInt64 = 0xcbf2_9ce4_8422_2325
    for byte in text.utf8 {
        hash ^= UInt64(byte)
        hash = hash &* 0x100_0000_01b3
    }
    return String(hash, radix: 16)
}
