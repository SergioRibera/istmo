import Foundation

/// The limits of `src/validate.rs` (the reference; its module docs hold
/// the table), checked by the iOS backend before UserNotifications sees a
/// value, so an invalid input fails with the same
/// `Invalid(InvalidField { field, problem })` as on desktop. Field paths
/// are Rust's (`actions[2].title`, `trigger.every`, `user_data`).
///
/// Characters are Unicode scalars (Rust `char`s), control = general
/// category Cc, blank = only `White_Space` scalars, byte lengths UTF-8.
enum NotificationsValidate {
    static let maxIdLen = 64
    static let maxTitleChars = 256
    static let maxBodyChars = 4096
    static let maxStatusChars = 64
    static let maxLabelChars = 128
    static let maxActions = 16
    static let maxChoices = 10
    static let maxInboxLines = 16
    static let maxMessages = 25
    static let maxUrlLen = 2048
    static let maxUserDataLen = 3 * 1024
    static let maxImageBytes = 2 * 1024 * 1024
    static let maxProgressMarks = 32
    static let maxVibrationSteps = 32
    static let minRepeatMs: UInt64 = 60_000
    static let minSnoozeMs: UInt64 = 1_000
    static let maxSnoozeMs: UInt64 = 30 * 24 * 3_600_000
    static let reservedPrefix = "istmo."
    static let callActions = ["istmo.call.answer", "istmo.call.decline", "istmo.call.hang_up"]

    typealias Problem = Notifications.Problem

    struct Failure: Error {
        let field: String
        let problem: Problem

        var error: Notifications.NotificationError {
            .invalid(Notifications.InvalidField(field: field, problem: problem))
        }
    }

    // MARK: rules

    /// At most `max` characters, no control characters (newlines and tabs
    /// pass when `multiline`).
    static func text(_ value: String, _ max: Int, multiline: Bool) -> Problem? {
        var count = 0
        for scalar in value.unicodeScalars {
            count += 1
            if count > max { return .tooLong }
            if scalar.properties.generalCategory == .control && !(multiline && (scalar == "\n" || scalar == "\t")) {
                return .badCharset
            }
        }
        return nil
    }

    /// `text`, and not empty (or only whitespace).
    static func label(_ value: String, _ max: Int) -> Problem? {
        if value.unicodeScalars.allSatisfy({ $0.properties.isWhitespace }) { return .empty }
        return text(value, max, multiline: false)
    }

    static func optional(_ value: String?, _ max: Int, multiline: Bool) -> Problem? {
        value.flatMap { text($0, max, multiline: multiline) }
    }

    /// An RFC 3986 scheme, then no whitespace or control characters.
    static func url(_ value: String) -> Problem? {
        if value.utf8.count > maxUrlLen { return .tooLong }
        guard let colon = value.utf8.firstIndex(of: UInt8(ascii: ":")) else { return .badUrl }
        let scheme = value.utf8[value.utf8.startIndex ..< colon]
        guard let first = scheme.first, isAsciiAlpha(first) else { return .badUrl }
        let rest = scheme.dropFirst().allSatisfy { b in
            isAsciiAlpha(b) || (b >= 0x30 && b <= 0x39) || b == UInt8(ascii: "+") || b == UInt8(ascii: "-")
                || b == UInt8(ascii: ".")
        }
        if !rest { return .badUrl }
        if value.unicodeScalars.contains(where: { $0.properties.isWhitespace || $0.properties.generalCategory == .control }) {
            return .badUrl
        }
        return nil
    }

    static func isAsciiAlpha(_ b: UInt8) -> Bool { (b >= 0x41 && b <= 0x5a) || (b >= 0x61 && b <= 0x7a) }

    static func image(_ value: Notifications.NotificationImage) -> Problem? {
        switch value {
        case .png(let bytes), .jpeg(let bytes):
            if bytes.isEmpty { return .empty }
            return bytes.count > maxImageBytes ? .tooLarge : nil
        case .file(let path):
            if path.isEmpty { return .empty }
            if !path.hasPrefix("/") { return .notAllowedHere }
            return text(path, maxUrlLen, multiline: false)
        case .named(let name):
            return label(name, maxLabelChars)
        }
    }

    /// The shared id rule: `1..=64` bytes of `[A-Za-z0-9._:-]`.
    static func id(_ value: String) -> Problem? {
        if value.isEmpty { return .empty }
        if value.utf8.count > maxIdLen { return .tooLong }
        let allowed = value.utf8.allSatisfy { b in
            isAsciiAlpha(b) || (b >= 0x30 && b <= 0x39) || b == UInt8(ascii: ".") || b == UInt8(ascii: "_")
                || b == UInt8(ascii: ":") || b == UInt8(ascii: "-")
        }
        return allowed ? nil : .badCharset
    }

    /// Throws `problem` (when there is one) for `field`.
    static func at(_ problem: Problem?, _ field: @autoclosure () -> String) throws {
        if let problem = problem { throw Failure(field: field(), problem: problem) }
    }

    // MARK: parts

    static func participant(_ value: Notifications.Participant, _ path: String) throws {
        try at(label(value.key, maxLabelChars), "\(path).key")
        try at(label(value.name, maxLabelChars), "\(path).name")
        if let avatar = value.avatar { try at(image(avatar), "\(path).avatar") }
    }

    static func message(_ value: Notifications.ChatMessage, _ path: String) throws {
        try at(label(value.text, maxBodyChars), "\(path).text")
        if let sender = value.sender { try participant(sender, "\(path).sender") }
        if let picture = value.image { try at(image(picture), "\(path).image") }
    }

    static func style(_ value: Notifications.Style, _ path: String) throws {
        switch value {
        case .plain:
            return
        case .bigText(let body):
            try at(text(body, maxBodyChars, multiline: true), path)
        case .inbox(let lines):
            if lines.count > maxInboxLines { throw Failure(field: path, problem: .tooLong) }
            for (i, line) in lines.enumerated() {
                try at(text(line, maxTitleChars, multiline: false), "\(path)[\(i)]")
            }
        case .picture(let picture):
            try at(image(picture), path)
        case .messaging(let conversation):
            try participant(conversation.user, "\(path).user")
            try at(optional(conversation.title, maxTitleChars, multiline: false), "\(path).title")
            for (i, msg) in conversation.messages.enumerated() {
                try message(msg, "\(path).messages[\(i)]")
            }
        case .call(let call):
            try participant(call.caller, "\(path).caller")
        }
    }

    static func progress(_ value: Notifications.Progress, _ path: String) throws {
        if let fraction = value.value, !(fraction.isFinite && fraction >= 0 && fraction <= 1) {
            throw Failure(field: "\(path).value", problem: .outOfRange)
        }
        try at(optional(value.label, maxLabelChars, multiline: false), "\(path).label")
        if value.segments.count > maxProgressMarks { throw Failure(field: "\(path).segments", problem: .tooLong) }
        if value.points.count > maxProgressMarks { throw Failure(field: "\(path).points", problem: .tooLong) }
    }

    static func choices(_ values: [String], _ path: String) throws {
        if values.count > maxChoices { throw Failure(field: path, problem: .tooLong) }
        var seen = Set<String>()
        for (i, choice) in values.enumerated() {
            try at(label(choice, maxLabelChars), "\(path)[\(i)]")
            if !seen.insert(choice).inserted { throw Failure(field: "\(path)[\(i)]", problem: .duplicate) }
        }
    }

    static func actions(_ values: [Notifications.Action], isCall: Bool) throws {
        if values.count > maxActions { throw Failure(field: "actions", problem: .tooLong) }
        var seen = Set<String>()
        for (i, action) in values.enumerated() {
            let id = action.id.value
            try at(Self.id(id), "actions[\(i)].id")
            if id.hasPrefix(reservedPrefix) && !(isCall && callActions.contains(id)) {
                throw Failure(field: "actions[\(i)].id", problem: .reserved)
            }
            if !seen.insert(id).inserted { throw Failure(field: "actions[\(i)].id", problem: .duplicate) }
            try at(label(action.title, maxLabelChars), "actions[\(i)].title")
            if let link = action.url { try at(url(link), "actions[\(i)].url") }
            if let icon = action.icon { try at(image(icon), "actions[\(i)].icon") }
            switch action.kind {
            case .button:
                break
            case .reply(let options):
                try at(optional(options.placeholder, maxLabelChars, multiline: false), "actions[\(i)].placeholder")
                try at(optional(options.submitTitle, maxLabelChars, multiline: false), "actions[\(i)].submit_title")
                try choices(options.suggestions, "actions[\(i)].suggestions")
            case .select(let list):
                if list.isEmpty { throw Failure(field: "actions[\(i)].choices", problem: .empty) }
                try choices(list, "actions[\(i)].choices")
            case .snooze(let ms):
                if ms < minSnoozeMs || ms > maxSnoozeMs {
                    throw Failure(field: "actions[\(i)].snooze", problem: .outOfRange)
                }
            }
        }
    }

    static func hints(_ values: [Notifications.LinuxHint]) throws {
        for (i, hint) in values.enumerated() {
            if hint.key.isEmpty { throw Failure(field: "linux_hints[\(i)].key", problem: .empty) }
            if hint.key.utf8.count > 64 || !hint.key.utf8.allSatisfy({ $0 < 0x80 }) {
                throw Failure(field: "linux_hints[\(i)].key", problem: .badCharset)
            }
            let size: Int
            switch hint.value {
            case .bool, .number: size = 0
            case .text(let text): size = text.utf8.count
            case .bytes(let bytes): size = bytes.count
            }
            if size > maxImageBytes { throw Failure(field: "linux_hints[\(i)].value", problem: .tooLarge) }
        }
    }

    static func isCall(_ style: Notifications.Style?) -> Bool {
        if case .call? = style { return true }
        return false
    }

    // MARK: entry points (each throws `NotificationError.invalid`)

    /// `Notification::validate`.
    static func check(_ n: Notifications.Notification) throws {
        do {
            try at(id(n.id.value), "id")
            if let channel = n.channel { try at(id(channel.value), "channel") }
            try at(label(n.title, maxTitleChars), "title")
            try at(optional(n.subtitle, maxTitleChars, multiline: false), "subtitle")
            try at(optional(n.body, maxBodyChars, multiline: true), "body")
            try style(n.style, "style")
            if let icon = n.icon { try at(image(icon), "icon") }
            try actions(n.actions, isCall: isCall(n.style))
            if let link = n.url { try at(url(link), "url") }
            if let data = n.userData, data.utf8.count > maxUserDataLen {
                throw Failure(field: "user_data", problem: .tooLarge)
            }
            if let group = n.group {
                try at(id(group.id.value), "group")
                try at(optional(group.title, maxTitleChars, multiline: false), "group.title")
                try at(optional(group.summary, maxTitleChars, multiline: false), "group.summary")
            }
            if let bar = n.progress { try progress(bar, "progress") }
            try at(optional(n.statusText, maxStatusChars, multiline: false), "status_text")
            if n.timeoutMs == 0 { throw Failure(field: "timeout_ms", problem: .outOfRange) }
            if let score = n.apple?.relevanceScore, !(score.isFinite && score >= 0 && score <= 1) {
                throw Failure(field: "apple.relevance_score", problem: .outOfRange)
            }
            try hints(n.linuxHints)
        } catch let failure as Failure {
            throw failure.error
        }
    }

    /// `NotificationPatch::validate`: the fields it sets, and no field
    /// both set and cleared. Call actions are refused only when the patch
    /// itself sets a non-call style (the patched notification is checked
    /// as a whole after it is applied).
    static func check(_ p: Notifications.NotificationPatch) throws {
        do {
            if let title = p.title { try at(label(title, maxTitleChars), "title") }
            try at(optional(p.subtitle, maxTitleChars, multiline: false), "subtitle")
            try at(optional(p.body, maxBodyChars, multiline: true), "body")
            if let value = p.style { try style(value, "style") }
            if let icon = p.icon { try at(image(icon), "icon") }
            if let list = p.actions { try actions(list, isCall: p.style == nil || isCall(p.style)) }
            if let bar = p.progress { try progress(bar, "progress") }
            try at(optional(p.statusText, maxStatusChars, multiline: false), "status_text")
            if let link = p.url { try at(url(link), "url") }
            if let data = p.userData, data.utf8.count > maxUserDataLen {
                throw Failure(field: "user_data", problem: .tooLarge)
            }
            for field in p.clear {
                let (set, name): (Bool, String)
                switch field {
                case .subtitle: (set, name) = (p.subtitle != nil, "subtitle")
                case .body: (set, name) = (p.body != nil, "body")
                case .icon: (set, name) = (p.icon != nil, "icon")
                case .progress: (set, name) = (p.progress != nil, "progress")
                case .statusText: (set, name) = (p.statusText != nil, "status_text")
                case .count: (set, name) = (p.count != nil, "count")
                case .url: (set, name) = (p.url != nil, "url")
                case .userData: (set, name) = (p.userData != nil, "user_data")
                case .actions: (set, name) = (p.actions != nil, "actions")
                }
                if set { throw Failure(field: name, problem: .notAllowedHere) }
            }
        } catch let failure as Failure {
            throw failure.error
        }
    }

    /// `Progress::validate`.
    static func check(_ bar: Notifications.Progress) throws {
        do { try progress(bar, "progress") } catch let failure as Failure { throw failure.error }
    }

    /// `ChatMessage::validate`.
    static func check(_ msg: Notifications.ChatMessage) throws {
        do { try message(msg, "message") } catch let failure as Failure { throw failure.error }
    }

    /// `Trigger::validate` / `CalendarRule::validate`; an IANA zone the
    /// device does not know is `trigger.time_zone` / `OutOfRange`, never a
    /// silent fall back to the device's zone.
    static func check(_ trigger: Notifications.Trigger) throws {
        func fail(_ field: String, _ problem: Problem) -> Notifications.NotificationError {
            Failure(field: field, problem: problem).error
        }
        switch trigger {
        case .after, .at:
            return
        case .every(let ms):
            if ms < minRepeatMs { throw fail("trigger.every", .outOfRange) }
        case .calendar(let rule):
            if rule.hour > 23 { throw fail("trigger.hour", .outOfRange) }
            if rule.minute > 59 { throw fail("trigger.minute", .outOfRange) }
            if let month = rule.month, !(1 ... 12).contains(month) { throw fail("trigger.month", .outOfRange) }
            if let day = rule.dayOfMonth {
                let max: UInt8
                switch rule.month {
                case 2?: max = 29
                case 4?, 6?, 9?, 11?: max = 30
                default: max = 31
                }
                if !(1 ... max).contains(day) { throw fail("trigger.day_of_month", .outOfRange) }
            }
            var seen = Set<UInt32>()
            for day in rule.weekdays where !seen.insert(day.rawValue).inserted {
                throw fail("trigger.weekdays", .duplicate)
            }
            if let zone = rule.timeZone, TimeZone(identifier: zone) == nil {
                throw fail("trigger.time_zone", .outOfRange)
            }
        }
    }

    /// `ChannelSpec::validate`, fields under `channels[<index>]` as
    /// `set_channels` reports them.
    static func check(_ spec: Notifications.ChannelSpec, index: Int) throws {
        do {
            do {
                try at(id(spec.id.value), "channel")
                try at(label(spec.name, maxLabelChars * 2), "name")
                try at(optional(spec.description, maxBodyChars, multiline: true), "description")
                if let steps = spec.vibration, steps.count > maxVibrationSteps {
                    throw Failure(field: "vibration", problem: .tooLong)
                }
                try at(optional(spec.group, maxLabelChars, multiline: false), "group")
            } catch let failure as Failure {
                throw Failure(field: "channels[\(index)].\(failure.field)", problem: failure.problem)
            }
        } catch let failure as Failure {
            throw failure.error
        }
    }
}
