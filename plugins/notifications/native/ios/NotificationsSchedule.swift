import Foundation
import IstmoRuntime
import UserNotifications

/// Scheduled notifications on iOS: native triggers, at most 64 pending
/// requests per app (the rest silently dropped by the system), so every
/// entry lives in this store and only the soonest `armedLimit` requests
/// are armed. Top-up runs at launch, on foreground, in the delegate
/// callbacks and after every change.
///
/// Trigger mapping: `After` = a one-shot interval; `At` = a one-shot
/// calendar date (UTC components); `Every` = a repeating interval (>= 60
/// s, starting when armed); `Calendar` = one repeating calendar trigger
/// per weekday (request ids `<id>#w<n>`), in the rule's zone. `until_ms`
/// is enforced when the plugin runs: an entry past it is removed at the
/// next top-up.
///
/// File: `<Application Support>/istmo-notifications/schedule.bin`, format
/// byte 1, then the wire encoding of `Vec<(Notification, Trigger, i64
/// anchor_ms)>` (generated codecs).
final class NotificationsScheduleStore: @unchecked Sendable {
    static let shared = NotificationsScheduleStore()
    static let format: UInt8 = 1
    /// Requests armed from the store; the rest of the 64 stay free for
    /// snoozes and immediate posts.
    static let armedLimit = 60

    struct Entry {
        let notification: Notifications.Notification
        let trigger: Notifications.Trigger
        let anchorMs: Int64
    }

    private let lock = NSLock()
    private var entries: [String: Entry] = [:]
    private var loaded = false
    private var core: NotificationsCore { NotificationsCore.shared }

    private var file: URL? {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first?
            .appendingPathComponent("istmo-notifications", isDirectory: true)
            .appendingPathComponent("schedule.bin")
    }

    private func ensureLoaded() {
        lock.guarded {
            if loaded { return }
            loaded = true
            guard let file = file, let data = try? Data(contentsOf: file), !data.isEmpty,
                  data[data.startIndex] == Self.format else { return }
            var c = Bincode.Cursor(data.subdata(in: (data.startIndex + 1) ..< data.endIndex))
            let codecs = core.codecs
            let decoded = try? Bincode.readVec(&c) { c1 -> Entry in
                let n = try codecs.readNotification(&c1)
                let trigger = try codecs.readTrigger(&c1)
                let anchor = try Bincode.readVarintI64(&c1)
                return Entry(notification: n, trigger: trigger, anchorMs: anchor)
            }
            guard let decoded = decoded else {
                NSLog("istmo-notifications: unreadable schedule store, ignored")
                return
            }
            for entry in decoded { entries[entry.notification.id.value] = entry }
        }
    }

    /// Writes the store; called with `lock` held.
    private func persistLocked() {
        guard let file = file else { return }
        if entries.isEmpty {
            try? FileManager.default.removeItem(at: file)
            return
        }
        var out = Data()
        out.append(Self.format)
        let codecs = core.codecs
        Bincode.writeVec(&out, entries.values.sorted { $0.anchorMs < $1.anchorMs }) { o, e in
            codecs.writeNotification(&o, e.notification)
            codecs.writeTrigger(&o, e.trigger)
            Bincode.writeVarintI64(&o, e.anchorMs)
        }
        do {
            try FileManager.default.createDirectory(
                at: file.deletingLastPathComponent(), withIntermediateDirectories: true, attributes: nil)
            try out.write(to: file, options: .atomic)
        } catch {
            NSLog("istmo-notifications: schedule store not written: \(error)")
        }
    }

    // MARK: triggers

    /// The UN triggers of `trigger` with their request ids.
    static func requests(_ id: String, _ trigger: Notifications.Trigger, anchorMs: Int64, nowMs: Int64)
        -> [(String, UNNotificationTrigger)]
    {
        switch trigger {
        case .after(let ms):
            let fire = anchorMs + Int64(clamping: ms)
            let left = max(Double(fire - nowMs) / 1000, 1)
            return [(id, UNTimeIntervalNotificationTrigger(timeInterval: left, repeats: false))]
        case .at(let ms):
            var calendar = Calendar(identifier: .gregorian)
            calendar.timeZone = TimeZone(identifier: "UTC") ?? calendar.timeZone
            let date = Date(timeIntervalSince1970: Double(ms) / 1000)
            let parts = calendar.dateComponents([.year, .month, .day, .hour, .minute, .second], from: date)
            var components = parts
            components.timeZone = calendar.timeZone
            return [(id, UNCalendarNotificationTrigger(dateMatching: components, repeats: false))]
        case .every(let ms):
            return [(id, UNTimeIntervalNotificationTrigger(timeInterval: max(Double(ms) / 1000, 60), repeats: true))]
        case .calendar(let rule):
            var base = DateComponents()
            base.hour = Int(rule.hour)
            base.minute = Int(rule.minute)
            base.day = rule.dayOfMonth.map { Int($0) }
            base.month = rule.month.map { Int($0) }
            if let zone = rule.timeZone { base.timeZone = TimeZone(identifier: zone) }
            if rule.weekdays.isEmpty {
                return [(id, UNCalendarNotificationTrigger(dateMatching: base, repeats: true))]
            }
            return rule.weekdays.map { day in
                var components = base
                // Foundation: 1 = Sunday ... 7 = Saturday.
                components.weekday = day == .sun ? 1 : Int(day.rawValue) + 2
                return ("\(id)#w\(day.rawValue)", UNCalendarNotificationTrigger(dateMatching: components, repeats: true))
            }
        }
    }

    /// The next fire of an entry after now, nil when it has none left.
    static func nextFire(_ e: Entry, nowMs: Int64) -> Int64? {
        switch e.trigger {
        case .after(let ms):
            let fire = e.anchorMs + Int64(clamping: ms)
            return fire > nowMs - 60_000 ? fire : nil
        case .at(let ms):
            return ms > nowMs - 60_000 ? ms : nil
        case .every(let ms):
            let period = max(Int64(clamping: ms), 60_000)
            let k = max((nowMs - e.anchorMs) / period + 1, 1)
            return e.anchorMs + k * period
        case .calendar(let rule):
            let next = requests(e.notification.id.value, e.trigger, anchorMs: e.anchorMs, nowMs: nowMs)
                .compactMap { ($0.1 as? UNCalendarNotificationTrigger)?.nextTriggerDate() }
                .map { Int64($0.timeIntervalSince1970 * 1000) }
                .min()
            guard let fire = next else { return nil }
            if let until = rule.untilMs, fire > until { return nil }
            return fire
        }
    }

    // MARK: API

    /// Stores and arms `n`. Rejects a trigger that never fires.
    func add(_ n: Notifications.Notification, _ trigger: Notifications.Trigger) async throws
        -> Notifications.ScheduledInfo
    {
        ensureLoaded()
        let now = nowMs()
        let entry = Entry(notification: n, trigger: trigger, anchorMs: now)
        guard let next = Self.nextFire(entry, nowMs: now) else {
            throw Notifications.NotificationError.invalid(
                Notifications.InvalidField(field: "trigger", problem: .outOfRange))
        }
        lock.guarded {
            entries[n.id.value] = entry
            persistLocked()
        }
        // Replaces whatever was armed under the old trigger.
        remove(requestsOf: n.id.value)
        await topUp()
        return Notifications.ScheduledInfo(exact: true, nextFireMs: next)
    }

    /// Forgets `id` and disarms its requests. `true` when it was stored.
    @discardableResult
    func remove(_ id: String) -> Bool {
        ensureLoaded()
        let had: Bool = lock.guarded {
            let had = entries.removeValue(forKey: id) != nil
            if had { persistLocked() }
            return had
        }
        remove(requestsOf: id)
        return had
    }

    private func remove(requestsOf id: String) {
        let ids = [id] + (0 ..< 7).map { "\(id)#w\($0)" }
        core.center.removePendingNotificationRequests(withIdentifiers: ids)
    }

    func removeAll(where match: (Notifications.Notification) -> Bool) {
        ensureLoaded()
        let ids: [String] = lock.guarded {
            let ids = entries.values.filter { match($0.notification) }.map { $0.notification.id.value }
            for id in ids { entries.removeValue(forKey: id) }
            if !ids.isEmpty { persistLocked() }
            return ids
        }
        for id in ids { remove(requestsOf: id) }
    }

    func pending() -> [Notifications.PendingNotification] {
        ensureLoaded()
        let now = nowMs()
        return lock.guarded { entries.values.map { $0 } }
            .map { e in
                Notifications.PendingNotification(
                    notification: e.notification, trigger: e.trigger, nextFireMs: Self.nextFire(e, nowMs: now))
            }
            .sorted { ($0.nextFireMs ?? .max) < ($1.nextFireMs ?? .max) }
    }

    /// Arms the soonest requests, disarms the rest, drops entries that
    /// fired for good.
    func topUp() async {
        ensureLoaded()
        let now = nowMs()
        let armed = await core.center.pendingNotificationRequests()
        let armedIds = Set(armed.map { $0.identifier })

        var wanted: [(Entry, String, UNNotificationTrigger)] = []
        var finished: [String] = []
        let snapshot: [Entry] = lock.guarded { Array(entries.values) }
        var ordered: [(Int64, Entry)] = []
        for e in snapshot {
            let id = e.notification.id.value
            let oneShot: Bool
            switch e.trigger {
            case .after, .at: oneShot = true
            default: oneShot = false
            }
            guard let fire = Self.nextFire(e, nowMs: now) else {
                finished.append(id)
                continue
            }
            // A one-shot that is no longer pending fired (or was delivered).
            if oneShot && fire <= now && !armedIds.contains(id) {
                finished.append(id)
                continue
            }
            ordered.append((fire, e))
        }
        ordered.sort { $0.0 < $1.0 }
        for (_, e) in ordered {
            let requests = Self.requests(e.notification.id.value, e.trigger, anchorMs: e.anchorMs, nowMs: now)
            if wanted.count + requests.count > Self.armedLimit { break }
            for (rid, trigger) in requests { wanted.append((e, rid, trigger)) }
        }
        if !finished.isEmpty {
            lock.guarded {
                for id in finished { entries.removeValue(forKey: id) }
                persistLocked()
            }
        }

        let wantedIds = Set(wanted.map { $0.1 })
        let storeIds: Set<String> = lock.guarded { Set(entries.keys) }
        // Disarm store requests that lost their slot (never snoozes, never
        // requests the plugin did not arm from the store).
        let stale = armed.filter { request in
            guard request.content.userInfo[NotificationsKeys.snoozed] == nil,
                  let id = request.content.userInfo[NotificationsKeys.id] as? String,
                  storeIds.contains(id) || finished.contains(id) else { return false }
            return !wantedIds.contains(request.identifier)
        }.map { $0.identifier }
        if !stale.isEmpty { core.center.removePendingNotificationRequests(withIdentifiers: stale) }

        for (e, rid, trigger) in wanted where !armedIds.contains(rid) {
            let n = e.notification
            let rendered = NotificationsRender.render(n, core.limits(for: n))
            let content = await core.content(for: n, rendered: rendered, quiet: false)
            do {
                try await core.center.add(UNNotificationRequest(identifier: rid, content: content, trigger: trigger))
            } catch {
                core.emit(.failed(Notifications.Failure(id: n.id, message: error.localizedDescription)))
            }
        }
    }
}
