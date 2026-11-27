import Foundation

/// `CalendarRule` math, the Swift port of `src/schedule/rule.rs` (like
/// Android's `CalendarRule.kt`), pinned by `tests/schedule_vectors.tsv`
/// (run by `tests/native/ios/check.sh`).
///
/// A wall-clock time on the days that match every set field. Resolution
/// to an instant follows RFC 5545 / Temporal "compatible": a time in a
/// DST gap moves forward by the gap (`02:30` on a one-hour spring-forward
/// day is `03:30`), a time in an overlap is the earlier instant, days that
/// do not exist are skipped, `untilMs` is inclusive, the search is bounded
/// to 401 years. `UNCalendarNotificationTrigger` resolves gaps its own way
/// (`02:30` fires at `03:00`), so the iOS backend arms one-shot date
/// triggers at the instants computed here instead.
///
/// Civil dates are computed by hand; offsets come from
/// `TimeZone.secondsFromGMT(for:)`. Plain Foundation: no UIKit, no
/// UserNotifications, no runtime.
enum NotificationsCalendar {
    static let searchYears = 401
    static let dayMs: Int64 = 86_400_000

    /// The rule's zone: its `timeZone` when set, else `local`; nil for a
    /// name Foundation does not know.
    static func zone(_ rule: Notifications.CalendarRule, local: TimeZone) -> TimeZone? {
        guard let name = rule.timeZone else { return local }
        return TimeZone(identifier: name)
    }

    /// The first occurrence strictly after `afterMs`, nil when there is none.
    static func nextAfter(_ rule: Notifications.CalendarRule, afterMs: Int64, local: TimeZone = .current) -> Int64? {
        let hour = Int64(rule.hour)
        let minute = Int64(rule.minute)
        if hour > 23 || minute > 59 { return nil }
        guard let tz = zone(rule, local: local) else { return nil }
        let weekdays = Set(rule.weekdays.map { Int($0.rawValue) })
        let wantMonth = rule.month.map { Int($0) }
        let wantDay = rule.dayOfMonth.map { Int($0) }
        // Start a day early: the previous day's time can still be ahead of
        // `after` around a DST transition at midnight.
        let startDays = floorDiv(afterMs + offset(tz, afterMs), dayMs) - 1
        var (year, month, firstDay) = civil(startDays)
        let lastYear = year + searchYears
        while year <= lastYear {
            if wantMonth == nil || wantMonth == month {
                let len = daysInMonth(year, month)
                // An empty range when the wanted day is before the start
                // or past the month's length.
                let low = wantDay ?? firstDay
                let high = wantDay.map { $0 >= firstDay ? min($0, len) : 0 } ?? len
                for day in stride(from: low, through: high, by: 1) where day <= len {
                    let number = daysFromCivil(year, month, day)
                    if !weekdays.isEmpty && !weekdays.contains(weekday(number)) { continue }
                    let wall = number * dayMs + hour * 3_600_000 + minute * 60_000
                    let at = resolve(tz, wall)
                    if at <= afterMs { continue }
                    if let until = rule.untilMs, at > until { return nil }
                    return at
                }
            }
            firstDay = 1
            month += 1
            if month > 12 {
                month = 1
                year += 1
            }
        }
        return nil
    }

    /// The next `count` occurrences after `fromMs`.
    static func nextOccurrences(_ rule: Notifications.CalendarRule, fromMs: Int64, count: Int,
                                local: TimeZone = .current) -> [Int64] {
        var out: [Int64] = []
        var after = fromMs
        while out.count < count, let next = nextAfter(rule, afterMs: after, local: local) {
            out.append(next)
            after = next
        }
        return out
    }

    /// Offset of `tz` at instant `ms`, in milliseconds.
    static func offset(_ tz: TimeZone, _ ms: Int64) -> Int64 {
        Int64(tz.secondsFromGMT(for: Date(timeIntervalSince1970: Double(ms) / 1000))) * 1000
    }

    /// The instant of the local wall time `wall` (local fields read as UTC
    /// milliseconds): the earlier one in an overlap, shifted forward by the
    /// gap inside a gap.
    static func resolve(_ tz: TimeZone, _ wall: Int64) -> Int64 {
        let before = offset(tz, wall - dayMs)
        let after = offset(tz, wall + dayMs)
        let early = wall - before
        let late = wall - after
        let earlyValid = offset(tz, early) == before
        let lateValid = offset(tz, late) == after
        switch (earlyValid, lateValid) {
        case (true, true): return min(early, late)
        case (true, false): return early
        case (false, true): return late
        // A gap: the offset before the transition moves the time forward
        // by the gap.
        case (false, false): return early
        }
    }

    /// Monday = 0 for day number `days` since 1970-01-01 (a Thursday).
    static func weekday(_ days: Int64) -> Int {
        Int(((days + 3) % 7 + 7) % 7)
    }

    static func daysInMonth(_ year: Int, _ month: Int) -> Int {
        switch month {
        case 2: return (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 ? 29 : 28
        case 4, 6, 9, 11: return 30
        default: return 31
        }
    }

    static func floorDiv(_ a: Int64, _ b: Int64) -> Int64 {
        let q = a / b
        return (a % b != 0 && (a < 0) != (b < 0)) ? q - 1 : q
    }

    /// Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant).
    static func daysFromCivil(_ year: Int, _ month: Int, _ day: Int) -> Int64 {
        let y = Int64(month <= 2 ? year - 1 : year)
        let era = floorDiv(y, 400)
        let yoe = y - era * 400
        let mp = Int64((month + 9) % 12)
        let doy = (153 * mp + 2) / 5 + Int64(day) - 1
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy
        return era * 146_097 + doe - 719_468
    }

    /// `(year, month, day)` of day number `days`.
    static func civil(_ days: Int64) -> (Int, Int, Int) {
        let z = days + 719_468
        let era = floorDiv(z, 146_097)
        let doe = z - era * 146_097
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100)
        let mp = (5 * doy + 2) / 153
        let day = Int(doy - (153 * mp + 2) / 5 + 1)
        let month = Int(mp < 10 ? mp + 3 : mp - 9)
        let year = Int(yoe + era * 400) + (month <= 2 ? 1 : 0)
        return (year, month, day)
    }
}

/// Trigger timing and missed deliveries, the port of `Trigger::next_after`
/// / `first_fire` (`src/schedule.rs`) and the lateness rule of
/// `src/schedule/engine.rs`.
enum NotificationsTiming {
    /// A delivery this much later than its time counts as missed
    /// (`LATE_AFTER_MS`).
    static let lateAfterMs: Int64 = 60_000
    /// Calendar occurrences walked forward to find the latest missed one
    /// (`MAX_MISSED_WALK`).
    static let maxMissedWalk = 4_096

    static func clamp(_ value: UInt64) -> Int64 { value > UInt64(Int64.max) ? .max : Int64(value) }

    /// First occurrence strictly after `afterMs` of `trigger` armed at `anchorMs`.
    static func nextAfter(_ trigger: Notifications.Trigger, anchorMs: Int64, afterMs: Int64,
                          local: TimeZone = .current) -> Int64? {
        switch trigger {
        case .after(let delay):
            let (at, overflow) = anchorMs.addingReportingOverflow(clamp(delay))
            let fire = overflow ? Int64.max : at
            return fire > afterMs ? fire : nil
        case .at(let at):
            return at > afterMs ? at : nil
        case .every(let period):
            let p = max(clamp(period), 1)
            let (difference, wrapped) = afterMs.subtractingReportingOverflow(anchorMs)
            let elapsed = wrapped ? (afterMs < anchorMs ? Int64.min : Int64.max) : difference
            let k = elapsed < 0 ? 1 : elapsed / p + 1
            let (step, mulOverflow) = k.multipliedReportingOverflow(by: p)
            if mulOverflow { return .max }
            let (at, overflow) = anchorMs.addingReportingOverflow(step)
            return overflow ? .max : at
        case .calendar(let rule):
            return NotificationsCalendar.nextAfter(rule, afterMs: max(afterMs, anchorMs), local: local)
        }
    }

    /// The first delivery of `trigger` armed at `anchorMs`; a past `At`
    /// delivers at once.
    static func firstFire(_ trigger: Notifications.Trigger, anchorMs: Int64, local: TimeZone = .current) -> Int64? {
        if case .at(let at) = trigger { return max(at, anchorMs) }
        return nextAfter(trigger, anchorMs: anchorMs, afterMs: anchorMs - 1, local: local)
    }

    static func repeats(_ trigger: Notifications.Trigger) -> Bool {
        switch trigger {
        case .every, .calendar: return true
        case .after, .at: return false
        }
    }

    /// The latest occurrence at or before `nowMs`, starting from the due
    /// one at `at`: lateness counts from the occurrence missed last, so a
    /// daily reminder missed for three days is as late as today's.
    static func latestOccurrence(_ trigger: Notifications.Trigger, anchorMs: Int64, at: Int64, nowMs: Int64,
                                 local: TimeZone = .current) -> Int64 {
        switch trigger {
        case .after, .at:
            return at
        case .every(let period):
            let p = max(clamp(period), 1)
            let steps = (nowMs - anchorMs) / p
            let (step, mulOverflow) = steps.multipliedReportingOverflow(by: p)
            let (latest, overflow) = anchorMs.addingReportingOverflow(mulOverflow ? .max : step)
            return max(overflow ? .max : latest, at)
        case .calendar:
            let next = { (after: Int64) in nextAfter(trigger, anchorMs: anchorMs, afterMs: after, local: local) }
            var last = at
            var back: Int64 = 1_000
            while true {
                let probe = nowMs - back
                if probe <= at { break }
                if let found = next(probe), found <= nowMs {
                    last = found
                    break
                }
                back = back > Int64.max / 2 ? .max : back * 2
            }
            for _ in 0 ..< maxMissedWalk {
                guard let found = next(last), found <= nowMs else { break }
                last = found
            }
            return last
        }
    }

    /// What an occurrence due at `at` and seen at `nowMs` turns into:
    /// nil = skipped (`MissedPolicy.drop`, or later than `deliverLate`
    /// allows), false = delivered on time (at most `lateAfterMs` late),
    /// true = delivered late.
    static func delivery(_ policy: Notifications.MissedPolicy, latestMs: Int64, nowMs: Int64) -> Bool? {
        let lateness = nowMs - latestMs
        if lateness <= lateAfterMs { return false }
        switch policy {
        case .drop: return nil
        case .deliverLate(let max): return lateness <= clamp(max) ? true : nil
        }
    }
}

/// The pure part of the iOS schedule store (`NotificationsSchedule.swift`):
/// which occurrences to arm and how an overdue entry settles. Plain
/// Foundation, checked by `tests/native/ios/check.sh`.
enum NotificationsPlan {
    struct Entry {
        let notification: Notifications.Notification
        let trigger: Notifications.Trigger
        let anchorMs: Int64
        /// The next occurrence not yet settled (nil: none left).
        var nextFireMs: Int64?
        /// The latest occurrence armed with the system (delivered by it
        /// even when the app does not run).
        var armedThroughMs: Int64?
    }

    static func requestId(_ id: String, _ trigger: Notifications.Trigger, fireMs: Int64) -> String {
        NotificationsTiming.repeats(trigger) ? "\(id)@\(fireMs)" : id
    }

    struct Occurrence {
        let fireMs: Int64
        let entry: Entry
        let requestId: String
    }

    /// The soonest `limit` occurrences of `snapshot` from their next fire
    /// on, ties by id.
    static func soonest(_ snapshot: [Entry], limit: Int, local: TimeZone = .current) -> [Occurrence] {
        guard limit > 0 else { return [] }
        let ordered = snapshot.filter { $0.nextFireMs != nil }.sorted {
            ($0.nextFireMs ?? .max, $0.notification.id.value) < ($1.nextFireMs ?? .max, $1.notification.id.value)
        }
        var out: [Occurrence] = []
        for e in ordered {
            let cutoff = out.count >= limit ? out[limit - 1].fireMs : Int64.max
            guard var fire = e.nextFireMs, fire <= cutoff else { break }
            var taken = 0
            while taken < limit, fire <= cutoff || out.count < limit {
                out.append(Occurrence(fireMs: fire, entry: e,
                                      requestId: requestId(e.notification.id.value, e.trigger, fireMs: fire)))
                taken += 1
                guard NotificationsTiming.repeats(e.trigger),
                      let next = NotificationsTiming.nextAfter(e.trigger, anchorMs: e.anchorMs, afterMs: fire,
                                                               local: local)
                else { break }
                fire = next
            }
            out.sort { ($0.fireMs, $0.entry.notification.id.value) < ($1.fireMs, $1.entry.notification.id.value) }
            if out.count > limit { out.removeSubrange(limit...) }
        }
        return out
    }

    /// What an overdue entry turns into at `nowMs`: the entry moved past
    /// now (nil when it has no occurrence left), and whether to post it
    /// now (`late` flag) under `MissedPolicy`.
    static func settle(_ e: Entry, nowMs: Int64, local: TimeZone = .current) -> (Entry?, late: Bool?) {
        guard var fire = e.nextFireMs else { return (nil, nil) }
        if fire > nowMs { return (e, nil) }
        // Occurrences up to what was armed were delivered by the system;
        // the ones after it, up to now, were missed.
        if let armed = e.armedThroughMs, fire <= armed {
            if armed >= nowMs {
                fire = Int64.max
            } else if let after = NotificationsTiming.nextAfter(e.trigger, anchorMs: e.anchorMs, afterMs: armed,
                                                                local: local) {
                fire = after
            } else {
                return (nil, nil)
            }
        }
        var late: Bool?
        if fire <= nowMs {
            let latest = NotificationsTiming.latestOccurrence(e.trigger, anchorMs: e.anchorMs, at: fire,
                                                               nowMs: nowMs, local: local)
            late = NotificationsTiming.delivery(e.notification.missed, latestMs: latest, nowMs: nowMs)
        }
        // Still-armed occurrences after now stay this entry's next ones.
        guard let next = NotificationsTiming.nextAfter(e.trigger, anchorMs: e.anchorMs, afterMs: nowMs, local: local)
        else { return (nil, late) }
        var moved = e
        moved.nextFireMs = next
        return (moved, late)
    }

}
