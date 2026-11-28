import Foundation
import IstmoRuntime
import UserNotifications

/// Scheduled notifications on iOS.
///
/// The system keeps at most 64 pending requests per app (the rest are
/// dropped silently), so every entry lives in this store and only the
/// soonest occurrences are armed: at most `armedLimit` (60) across all
/// entries, fewer while snoozes hold slots (snoozes + armed <= 64). Each
/// armed request is one occurrence, a one-shot date trigger at the instant
/// `NotificationsTiming` computes (the Rust rules: calendar DST gaps,
/// `Every` on its anchor's grid, `until_ms` exact), never a repeating
/// `UN*Trigger`. Request ids: `<id>` for `After` / `At`,
/// `<id>@<fire ms>` for each occurrence of a repeat, `<id>#s` for a
/// snoozed copy.
///
/// Top-up (serialized: one at a time with every other store change) runs
/// at launch, on foreground, in the delegate callbacks and after every
/// change. It first settles what came due: occurrences the system had
/// armed were delivered by it; ones past what was armed (no slot while
/// the app was not running) were missed and go through `MissedPolicy`
/// like the Rust engine (more than 60 s late = missed, lateness from the
/// latest missed occurrence, `Drop` skips, `DeliverLate(max)` posts once
/// with `DeliveredLate`). Then it arms the soonest occurrences and
/// removes every pending request of ours that is not wanted (also ones
/// without a store entry, left by a cancel or an earlier process).
///
/// File: `<Application Support>/istmo-notifications/schedule.bin`, format
/// byte 2, then the wire encoding of `Vec<(Notification, Trigger, i64
/// anchor_ms, Option<i64> next_fire_ms, Option<i64> armed_through_ms)>`
/// (generated codecs). A file of another format is ignored.
final class NotificationsScheduleStore: @unchecked Sendable {
    static let shared = NotificationsScheduleStore()
    static let format: UInt8 = 2
    /// The system's cap on pending requests.
    static let pendingCap = 64
    /// Requests armed from the store; the rest of the 64 stay free for
    /// snoozes.
    static let armedLimit = 60
    static let snoozeSuffix = "#s"

    typealias Entry = NotificationsPlan.Entry

    private let lock = NSLock()
    private var entries: [String: Entry] = [:]
    private var loaded = false
    /// Every store change and top-up, one at a time.
    private let serial = NotificationsSerial()
    private static let lane = "schedule"
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
                let next = try Bincode.readOption(&c1) { c2 in try Bincode.readVarintI64(&c2) }
                let armed = try Bincode.readOption(&c1) { c2 in try Bincode.readVarintI64(&c2) }
                return Entry(notification: n, trigger: trigger, anchorMs: anchor, nextFireMs: next,
                             armedThroughMs: armed)
            }
            guard let decoded = decoded else {
                NSLog("istmo-notifications: unreadable schedule store, ignored")
                return
            }
            for entry in decoded where entry.nextFireMs != nil { entries[entry.notification.id.value] = entry }
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
        let ordered = entries.values.sorted {
            ($0.nextFireMs ?? .max, $0.notification.id.value) < ($1.nextFireMs ?? .max, $1.notification.id.value)
        }
        Bincode.writeVec(&out, ordered) { o, e in
            codecs.writeNotification(&o, e.notification)
            codecs.writeTrigger(&o, e.trigger)
            Bincode.writeVarintI64(&o, e.anchorMs)
            Bincode.writeOption(&o, e.nextFireMs) { o1, v in Bincode.writeVarintI64(&o1, v) }
            Bincode.writeOption(&o, e.armedThroughMs) { o1, v in Bincode.writeVarintI64(&o1, v) }
        }
        do {
            try FileManager.default.createDirectory(
                at: file.deletingLastPathComponent(), withIntermediateDirectories: true, attributes: nil)
            try out.write(to: file, options: .atomic)
        } catch {
            NSLog("istmo-notifications: schedule store not written: \(error)")
        }
    }

    // MARK: request ids

    static func isSnooze(_ request: UNNotificationRequest) -> Bool {
        request.identifier.hasSuffix(snoozeSuffix) || request.content.userInfo[NotificationsKeys.snoozed] != nil
    }

    /// A one-shot trigger at `fireMs` (whole seconds, rounded up so it
    /// never fires early), as UTC date components: the system keeps it
    /// across reboots and clock changes.
    static func trigger(at fireMs: Int64) -> UNNotificationTrigger {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC") ?? calendar.timeZone
        let seconds = (Double(fireMs) / 1000).rounded(.up)
        let date = Date(timeIntervalSince1970: seconds)
        var components = calendar.dateComponents([.year, .month, .day, .hour, .minute, .second], from: date)
        components.timeZone = calendar.timeZone
        return UNCalendarNotificationTrigger(dateMatching: components, repeats: false)
    }

    // MARK: API

    /// Stores `n` and arms it. The trigger is already validated; one that
    /// never fires is refused like on desktop.
    func add(_ n: Notifications.Notification, _ trigger: Notifications.Trigger) async throws
        -> Notifications.ScheduledInfo
    {
        let now = nowMs()
        guard let next = NotificationsTiming.firstFire(trigger, anchorMs: now) else {
            var field = "trigger"
            if case .calendar(let rule) = trigger, rule.untilMs != nil { field = "trigger.until_ms" }
            throw Notifications.NotificationError.invalid(
                Notifications.InvalidField(field: field, problem: .outOfRange))
        }
        let id = n.id.value
        await serial.run(Self.lane) {
            ensureLoaded()
            lock.guarded {
                entries[id] = Entry(notification: n, trigger: trigger, anchorMs: now, nextFireMs: next,
                                    armedThroughMs: nil)
                persistLocked()
            }
            // Whatever was armed under the old trigger, snoozes included.
            await removeRequestsLocked(of: [id])
            await topUpLocked()
        }
        return Notifications.ScheduledInfo(exact: true, nextFireMs: next)
    }

    /// Forgets `id` and disarms its requests. `true` when it was stored.
    @discardableResult
    func remove(_ id: String) async -> Bool {
        await serial.run(Self.lane) {
            ensureLoaded()
            let had: Bool = lock.guarded {
                let had = entries.removeValue(forKey: id) != nil
                if had { persistLocked() }
                return had
            }
            await removeRequestsLocked(of: [id])
            return had
        }
    }

    /// Forgets every entry `match` selects (and `extra` ids) and disarms
    /// their requests.
    func removeAll(extra: [String] = [], where match: (Notifications.Notification) -> Bool) async {
        await serial.run(Self.lane) {
            ensureLoaded()
            let ids: [String] = lock.guarded {
                let ids = entries.values.filter { match($0.notification) }.map { $0.notification.id.value }
                for id in ids { entries.removeValue(forKey: id) }
                if !ids.isEmpty { persistLocked() }
                return ids
            }
            await removeRequestsLocked(of: Set(ids + extra))
        }
    }

    /// Disarms every pending request of this plugin (`cancel_all`).
    func removeAllRequests() async {
        await serial.run(Self.lane) {
            let pending = await core.center.pendingNotificationRequests()
            let ours = pending.filter { $0.content.userInfo[NotificationsKeys.id] is String }.map { $0.identifier }
            if !ours.isEmpty { core.center.removePendingNotificationRequests(withIdentifiers: ours) }
        }
    }

    /// Disarms every pending request of `ids`; called in the lane.
    private func removeRequestsLocked(of ids: Set<String>) async {
        guard !ids.isEmpty else { return }
        let pending = await core.center.pendingNotificationRequests()
        let gone = pending.compactMap { request -> String? in
            guard let owner = request.content.userInfo[NotificationsKeys.id] as? String, ids.contains(owner) else {
                return nil
            }
            return request.identifier
        }
        if !gone.isEmpty { core.center.removePendingNotificationRequests(withIdentifiers: gone) }
    }

    func pending() -> [Notifications.PendingNotification] {
        ensureLoaded()
        return lock.guarded { Array(entries.values) }
            .sorted { ($0.nextFireMs ?? .max, $0.notification.id.value) < ($1.nextFireMs ?? .max, $1.notification.id.value) }
            .map { e in
                Notifications.PendingNotification(notification: e.notification, trigger: e.trigger,
                                                  nextFireMs: e.nextFireMs)
            }
    }

    /// Settles what came due, arms the soonest occurrences, disarms the
    /// rest.
    func topUp() async {
        await serial.run(Self.lane) { await topUpLocked() }
    }

    /// Re-adds `content` (a delivered notification of `id`) as its snoozed
    /// copy `<id>#s` after `ms`, freeing a slot first when the 64 are
    /// taken.
    func snooze(_ id: String, _ content: UNNotificationContent, afterMs ms: UInt64) async throws {
        try await serial.run(Self.lane) {
            let pending = await core.center.pendingNotificationRequests()
            let snoozes = pending.filter { Self.isSnooze($0) && $0.identifier != id + Self.snoozeSuffix }.count
            if snoozes >= Self.pendingCap {
                throw Notifications.NotificationError.tooMany(UInt32(Self.pendingCap))
            }
            // Room for this one: armed occurrences give way.
            await topUpLocked(extraSnoozes: 1)
            let trigger = UNTimeIntervalNotificationTrigger(timeInterval: max(Double(ms) / 1000, 1), repeats: false)
            try await core.center.add(UNNotificationRequest(identifier: id + Self.snoozeSuffix, content: content,
                                                            trigger: trigger))
        }
    }

    /// The top-up itself; called in the lane. `extraSnoozes` = slots a
    /// snooze about to be added needs.
    private func topUpLocked(extraSnoozes: Int = 0) async {
        ensureLoaded()
        let now = nowMs()
        let snapshot: [Entry] = lock.guarded { Array(entries.values) }

        // 1. What came due.
        var due: [(Notifications.Notification, Bool)] = []
        var settled: [String: Entry?] = [:]
        for e in snapshot {
            guard let fire = e.nextFireMs, fire <= now else { continue }
            let (moved, late) = NotificationsPlan.settle(e, nowMs: now)
            settled[e.notification.id.value] = moved
            if let late = late { due.append((e.notification, late)) }
        }
        if !settled.isEmpty {
            lock.guarded {
                for (id, moved) in settled { entries[id] = moved }
                persistLocked()
            }
        }
        for (n, late) in due { await core.postScheduled(n, late: late) }

        // 2. The soonest occurrences, within the slots snoozes leave.
        let pending = await core.center.pendingNotificationRequests()
        let snoozes = pending.filter(Self.isSnooze).count + extraSnoozes
        let limit = max(0, min(Self.armedLimit, Self.pendingCap - snoozes))
        let current: [Entry] = lock.guarded { Array(entries.values) }
        let wanted = NotificationsPlan.soonest(current, limit: limit)
        let wantedIds = Set(wanted.map { $0.requestId })
        let pendingIds = Set(pending.map { $0.identifier })

        // 3. Ours that are not wanted go, entry or not.
        let stale = pending.filter { request in
            request.content.userInfo[NotificationsKeys.id] is String && !Self.isSnooze(request)
                && !wantedIds.contains(request.identifier)
        }.map { $0.identifier }
        if !stale.isEmpty { core.center.removePendingNotificationRequests(withIdentifiers: stale) }

        // 4. Arm what is missing; record how far each entry is armed.
        var armedThrough: [String: Int64] = [:]
        for occurrence in wanted {
            let n = occurrence.entry.notification
            let id = n.id.value
            var armed = pendingIds.contains(occurrence.requestId)
            if !armed {
                let content = await core.scheduledContent(for: n)
                do {
                    try await core.center.add(UNNotificationRequest(
                        identifier: occurrence.requestId, content: content.content,
                        trigger: Self.trigger(at: occurrence.fireMs)))
                    armed = true
                } catch {
                    content.discardFiles()
                    core.emit(.failed(Notifications.Failure(id: n.id, message: error.localizedDescription)))
                }
            }
            if armed { armedThrough[id] = max(armedThrough[id] ?? .min, occurrence.fireMs) }
        }
        lock.guarded {
            var changed = false
            for (id, e) in entries where e.armedThroughMs != armedThrough[id] {
                entries[id]?.armedThroughMs = armedThrough[id]
                changed = true
            }
            if changed { persistLocked() }
        }
    }
}

/// One async body at a time per key, in arrival order (the iOS
/// counterpart of the desktop host's per-id lanes). Bodies never hold
/// `lock` across an `await`.
final class NotificationsSerial: @unchecked Sendable {
    private let lock = NSLock()
    private var busy: Set<String> = []
    private var waiters: [String: [CheckedContinuation<Void, Never>]] = [:]

    func run<T>(_ key: String, _ body: () async throws -> T) async rethrows -> T {
        await acquire(key)
        defer { release(key) }
        return try await body()
    }

    private func acquire(_ key: String) async {
        await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
            let now: Bool = lock.guarded {
                if busy.contains(key) {
                    waiters[key, default: []].append(cont)
                    return false
                }
                busy.insert(key)
                return true
            }
            if now { cont.resume() }
        }
    }

    /// Hands the key to the next waiter (it stays busy), else frees it.
    private func release(_ key: String) {
        let next: CheckedContinuation<Void, Never>? = lock.guarded {
            if var queue = waiters[key], !queue.isEmpty {
                let first = queue.removeFirst()
                waiters[key] = queue.isEmpty ? nil : queue
                return first
            }
            busy.remove(key)
            return nil
        }
        next?.resume()
    }
}
