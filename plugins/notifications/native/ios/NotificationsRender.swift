import Foundation
import IstmoRuntime

/// What the Rust desktop host does in `src/render.rs`, for the iOS
/// backend: simulated lines in the body, buttons for choices, a "More"
/// action for what does not fit, the plugin's call actions, and the keys
/// each platform action carries back. `src/render.rs` is the reference.
enum NotificationsRender {
    static let moreKey = "istmo.more"
    static let callAnswer = "istmo.call.answer"
    static let callDecline = "istmo.call.decline"
    static let callHangUp = "istmo.call.hang_up"
    /// Messages of a conversation shown as body lines without a layout.
    static let simulatedMessages = 5

    /// What the platform shows natively (Apple: subtitle and inline reply;
    /// a conversation layout only for communication notifications).
    struct Limits {
        var maxActions: Int
        var messaging: Bool
        var more = "More"
        var answer = "Answer"
        var decline = "Decline"
        var hangUp = "Hang up"
    }

    enum Input {
        case none
        case reply(placeholder: String?, submit: String?)
        /// Simulated: the button opens the app, which shows its field.
        case requested
    }

    struct Button {
        let key: String
        let title: String
        let input: Input
        let opensApp: Bool
        let destructive: Bool
        let requiresUnlock: Bool
        /// Response fields: the action id (nil for "More"), the choice
        /// this button stands for, the URL to open.
        let actionId: String?
        let choice: String?
        let url: String?
        let snoozeMs: UInt64?
    }

    struct Rendered {
        let body: String?
        let buttons: [Button]
        let overflow: [Notifications.Action]
    }

    /// `<label> 42%`, `42%`, or `<label>`; nil for a bare indeterminate bar.
    static func progressText(_ p: Notifications.Progress) -> String? {
        let percent: Int? = p.value.map { v in
            let clamped = v.isFinite ? min(max(v, 0), 1) : 0
            return Int((clamped * 100).rounded(.down))
        }
        switch (p.label, percent) {
        case let (label?, percent?): return "\(label) \(percent)%"
        case let (nil, percent?): return "\(percent)%"
        case let (label?, nil): return label
        case (nil, nil): return nil
        }
    }

    /// Body lines on Apple (subtitle has its own slot, no progress bar).
    static func body(_ n: Notifications.Notification, _ limits: Limits) -> String? {
        var lines: [String] = []
        switch n.style {
        case .bigText(let text):
            lines.append(text)
        case .inbox(let entries):
            if let body = n.body { lines.append(body) }
            lines.append(contentsOf: entries)
        case .messaging(let conversation) where !limits.messaging:
            if let body = n.body { lines.append(body) }
            for message in conversation.messages.suffix(simulatedMessages) {
                let name = message.sender?.name ?? conversation.user.name
                lines.append("\(name): \(message.text)")
            }
        default:
            if let body = n.body { lines.append(body) }
        }
        if let progress = n.progress, let text = progressText(progress) { lines.append(text) }
        let joined = lines.filter { !$0.isEmpty }.joined(separator: "\n")
        return joined.isEmpty ? nil : joined
    }

    private static func callActions(_ kind: Notifications.CallKind, _ limits: Limits) -> [Notifications.Action] {
        func button(_ id: String, _ title: String, _ destructive: Bool) -> Notifications.Action {
            Notifications.Action(
                id: Notifications.ActionId(value: id), title: title, kind: .button, opensApp: true,
                destructive: destructive, requiresUnlock: false, url: nil, icon: nil)
        }
        switch kind {
        case .incoming, .screening:
            return [button(callDecline, limits.decline, true), button(callAnswer, limits.answer, false)]
        case .ongoing:
            return [button(callHangUp, limits.hangUp, true)]
        }
    }

    private static func expand(_ a: Notifications.Action, into out: inout [Button]) {
        func make(_ key: String, _ title: String, _ input: Input, _ opensApp: Bool,
                  choice: String? = nil, snooze: UInt64? = nil) -> Button {
            Button(key: key, title: title, input: input, opensApp: opensApp, destructive: a.destructive,
                   requiresUnlock: a.requiresUnlock, actionId: a.id.value, choice: choice, url: a.url,
                   snoozeMs: snooze)
        }
        switch a.kind {
        case .button:
            out.append(make(a.id.value, a.title, .none, a.opensApp))
        case .reply(let options):
            out.append(make(a.id.value, a.title,
                            .reply(placeholder: options.placeholder, submit: options.submitTitle), a.opensApp))
        case .select(let choices):
            for (index, choice) in choices.enumerated() {
                out.append(make("\(a.id.value)#\(index)", choice, .none, a.opensApp, choice: choice))
            }
        case .snooze(let ms):
            out.append(make(a.id.value, a.title, .none, false, snooze: ms))
        }
    }

    static func render(_ n: Notifications.Notification, _ limits: Limits) -> Rendered {
        var actions = n.actions
        if case .call(let call) = n.style,
           !actions.contains(where: { $0.id.value.hasPrefix("istmo.") }) {
            actions = callActions(call.kind, limits) + actions
        }
        var buttons: [Button] = []
        var firstButton: [Int] = []
        for action in actions {
            firstButton.append(buttons.count)
            expand(action, into: &buttons)
        }
        var overflow: [Notifications.Action] = []
        let max = limits.maxActions
        if buttons.count > max {
            if max == 0 {
                buttons.removeAll()
                overflow = actions
            } else {
                let keep = max - 1
                let endOf = { (i: Int) in i + 1 < firstButton.count ? firstButton[i + 1] : buttons.count }
                var cut = 0
                while cut < actions.count && endOf(cut) <= keep { cut += 1 }
                let kept = cut < firstButton.count ? firstButton[cut] : buttons.count
                buttons.removeSubrange(kept...)
                overflow = Array(actions[cut...])
                buttons.append(Button(key: moreKey, title: limits.more, input: .none, opensApp: true,
                                      destructive: false, requiresUnlock: false, actionId: nil, choice: nil,
                                      url: nil, snoozeMs: nil))
            }
        }
        return Rendered(body: body(n, limits), buttons: buttons, overflow: overflow)
    }
}

/// The response facts of one shown notification, kept in its `userInfo`
/// so a tap resolves the same way in this process and after a cold start.
struct NotificationsKeyMap {
    struct Entry {
        let key: String
        let actionId: String?
        let choice: String?
        let url: String?
        let background: Bool
        let snoozeMs: UInt64?
        let inputRequested: Bool
    }

    var entries: [Entry]
    /// Wire bytes of the overflow actions (images stripped).
    var overflow: Data

    init(_ rendered: NotificationsRender.Rendered, codecs: NotificationsCodecs) {
        entries = rendered.buttons.map { b in
            var requested = false
            if case .requested = b.input { requested = true }
            return Entry(key: b.key, actionId: b.actionId, choice: b.choice, url: b.url,
                         background: !b.opensApp, snoozeMs: b.snoozeMs, inputRequested: requested)
        }
        var out = Data()
        Bincode.writeVec(&out, rendered.overflow.map(Self.withoutIcon)) { o, a in codecs.writeAction(&o, a) }
        overflow = out
    }

    private static func withoutIcon(_ a: Notifications.Action) -> Notifications.Action {
        Notifications.Action(id: a.id, title: a.title, kind: a.kind, opensApp: a.opensApp,
                             destructive: a.destructive, requiresUnlock: a.requiresUnlock, url: a.url, icon: nil)
    }

    func encode() -> Data {
        var out = Data()
        Bincode.writeVec(&out, entries) { o, e in
            Bincode.writeString(&o, e.key)
            Bincode.writeOption(&o, e.actionId) { o1, v in Bincode.writeString(&o1, v) }
            Bincode.writeOption(&o, e.choice) { o1, v in Bincode.writeString(&o1, v) }
            Bincode.writeOption(&o, e.url) { o1, v in Bincode.writeString(&o1, v) }
            Bincode.writeBool(&o, e.background)
            Bincode.writeOption(&o, e.snoozeMs) { o1, v in Bincode.writeVarintU64(&o1, v) }
            Bincode.writeBool(&o, e.inputRequested)
        }
        Bincode.writeData(&out, overflow)
        return out
    }

    static func decode(_ data: Data) -> NotificationsKeyMap? {
        var c = Bincode.Cursor(data)
        guard let entries = try? Bincode.readVec(&c, { c1 -> Entry in
            let key = try Bincode.readString(&c1)
            let actionId = try Bincode.readOption(&c1) { c2 in try Bincode.readString(&c2) }
            let choice = try Bincode.readOption(&c1) { c2 in try Bincode.readString(&c2) }
            let url = try Bincode.readOption(&c1) { c2 in try Bincode.readString(&c2) }
            let background = try Bincode.readBool(&c1)
            let snooze = try Bincode.readOption(&c1) { c2 in try Bincode.readVarintU64(&c2) }
            let requested = try Bincode.readBool(&c1)
            return Entry(key: key, actionId: actionId, choice: choice, url: url, background: background,
                         snoozeMs: snooze, inputRequested: requested)
        }), let overflow = try? Bincode.readData(&c) else { return nil }
        return NotificationsKeyMap(entries: entries, overflow: overflow)
    }

    private init(entries: [Entry], overflow: Data) {
        self.entries = entries
        self.overflow = overflow
    }

    func entry(_ key: String) -> Entry? { entries.first { $0.key == key } }
}

extension Notifications.Notification {
    /// `Notification::apply`: every set field replaces, then `clear` removes.
    func applying(_ p: Notifications.NotificationPatch) -> Notifications.Notification {
        var subtitle = p.subtitle ?? self.subtitle
        var body = p.body ?? self.body
        var icon = p.icon ?? self.icon
        var actions = p.actions ?? self.actions
        var progress = p.progress ?? self.progress
        var statusText = p.statusText ?? self.statusText
        var count = p.count ?? self.count
        var url = p.url ?? self.url
        var userData = p.userData ?? self.userData
        for field in p.clear {
            switch field {
            case .subtitle: subtitle = nil
            case .body: body = nil
            case .icon: icon = nil
            case .progress: progress = nil
            case .statusText: statusText = nil
            case .count: count = nil
            case .url: url = nil
            case .userData: userData = nil
            case .actions: actions = []
            }
        }
        return with(title: p.title ?? title, subtitle: subtitle, body: body, style: p.style ?? style,
                    icon: icon, actions: actions, progress: progress, statusText: statusText, count: count,
                    ongoing: p.ongoing ?? ongoing, url: url, userData: userData)
    }

    /// A copy with the content fields replaced.
    func with(title: String, subtitle: String?, body: String?, style: Notifications.Style,
              icon: Notifications.NotificationImage?, actions: [Notifications.Action],
              progress: Notifications.Progress?, statusText: String?, count: UInt32?, ongoing: Bool,
              url: String?, userData: String?) -> Notifications.Notification {
        Notifications.Notification(
            id: id, channel: channel, title: title, subtitle: subtitle, body: body, style: style, icon: icon,
            actions: actions, url: url, userData: userData, group: group, category: category,
            interruption: interruption, sound: sound, progress: progress, ongoing: ongoing, live: live,
            statusText: statusText, timeoutMs: timeoutMs, timestampMs: timestampMs, chronometer: chronometer,
            count: count, lockScreen: lockScreen, hideInForeground: hideInForeground, missed: missed,
            android: android, apple: apple, linuxHints: linuxHints)
    }

    /// A copy with the channel's defaults merged in (interruption, sound,
    /// lock screen), as the desktop host does.
    func merging(_ spec: Notifications.ChannelSpec?) -> Notifications.Notification {
        guard let spec = spec else { return self }
        return Notifications.Notification(
            id: id, channel: channel, title: title, subtitle: subtitle, body: body, style: style, icon: icon,
            actions: actions, url: url, userData: userData, group: group, category: category,
            interruption: interruption ?? spec.importance, sound: sound ?? spec.sound, progress: progress,
            ongoing: ongoing, live: live, statusText: statusText, timeoutMs: timeoutMs,
            timestampMs: timestampMs, chronometer: chronometer, count: count,
            lockScreen: lockScreen ?? spec.lockScreen, hideInForeground: hideInForeground, missed: missed,
            android: android, apple: apple, linuxHints: linuxHints)
    }

    /// `Notification::normalize`: a conversation keeps its last 25
    /// messages.
    func normalized() -> Notifications.Notification {
        guard case .messaging(let c) = style, c.messages.count > NotificationsValidate.maxMessages else { return self }
        let conversation = Notifications.Conversation(user: c.user, title: c.title, isGroup: c.isGroup,
                                                      messages: Array(c.messages.suffix(NotificationsValidate.maxMessages)))
        return with(title: title, subtitle: subtitle, body: body, style: .messaging(conversation), icon: icon,
                    actions: actions, progress: progress, statusText: statusText, count: count,
                    ongoing: ongoing, url: url, userData: userData)
    }

    /// Keeps the last 25 messages of a conversation (`Notification::normalize`).
    func appending(_ message: Notifications.ChatMessage) -> Notifications.Notification? {
        guard case .messaging(let c) = style else { return nil }
        let messages = Array((c.messages + [message]).suffix(25))
        let conversation = Notifications.Conversation(user: c.user, title: c.title, isGroup: c.isGroup,
                                                      messages: messages)
        return with(title: title, subtitle: subtitle, body: body, style: .messaging(conversation), icon: icon,
                    actions: actions, progress: progress, statusText: statusText, count: count,
                    ongoing: ongoing, url: url, userData: userData)
    }
}
