// Checks of the plain-Foundation parts of the iOS backend: the calendar
// rule against `tests/schedule_vectors.tsv`, trigger timing, missed
// deliveries, the store's arming plan and the validation port (cases of
// `src/validate.rs`). Run by `check.sh` with the real Foundation.
import Foundation

var failures: [String] = []

func check(_ name: String, _ ok: Bool, _ detail: @autoclosure () -> String = "") {
    if !ok { failures.append("\(name) \(detail())") }
}

func eq<T: Equatable>(_ name: String, _ got: T, _ want: T) {
    check(name, got == want, "expected \(want), got \(got)")
}

let iso = ISO8601DateFormatter()

func ms(_ text: String) -> Int64 {
    guard let date = iso.date(from: text) else { fatalError("bad instant \(text)") }
    return Int64((date.timeIntervalSince1970 * 1000).rounded())
}

func fmt(_ list: [Int64]) -> String {
    list.isEmpty ? "none" : list.map { iso.string(from: Date(timeIntervalSince1970: Double($0) / 1000)) }
        .joined(separator: ",")
}

func opt(_ text: String) -> String? { text == "-" ? nil : text }

func weekday(_ text: String) -> Notifications.Weekday {
    switch text {
    case "mon": return .mon
    case "tue": return .tue
    case "wed": return .wed
    case "thu": return .thu
    case "fri": return .fri
    case "sat": return .sat
    case "sun": return .sun
    default: fatalError("bad weekday \(text)")
    }
}

func rule(_ hour: UInt8, _ minute: UInt8, zone: String? = "UTC", day: UInt8? = nil, month: UInt8? = nil,
          weekdays: [Notifications.Weekday] = [], until: Int64? = nil) -> Notifications.CalendarRule {
    Notifications.CalendarRule(hour: hour, minute: minute, weekdays: weekdays, dayOfMonth: day, month: month,
                               timeZone: zone, untilMs: until)
}

func vectors(_ path: String) {
    guard let table = try? String(contentsOfFile: path, encoding: .utf8) else { fatalError("no \(path)") }
    var rows = 0
    for line in table.split(separator: "\n", omittingEmptySubsequences: true) {
        if line.hasPrefix("#") || line.hasPrefix("name\t") || line.allSatisfy({ $0 == " " }) { continue }
        let c = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
        check("row", c.count == 12, line.description)
        if c.count != 12 { continue }
        let r = Notifications.CalendarRule(
            hour: UInt8(c[3])!, minute: UInt8(c[4])!,
            weekdays: opt(c[5])?.split(separator: ",").map { weekday(String($0)) } ?? [],
            dayOfMonth: opt(c[6]).map { UInt8($0)! }, month: opt(c[7]).map { UInt8($0)! }, timeZone: opt(c[1]),
            untilMs: opt(c[8]).map(ms))
        guard let local = TimeZone(identifier: c[2]) else {
            failures.append("vector \(c[0]): unknown local zone \(c[2])")
            continue
        }
        let got = fmt(NotificationsCalendar.nextOccurrences(r, fromMs: ms(c[9]), count: Int(c[10])!, local: local))
        eq("vector \(c[0])", got, c[11])
        rows += 1
    }
    check("vector rows", rows >= 30, "only \(rows)")
    print("schedule vectors: \(rows) rows")
}

let utc = TimeZone(identifier: "UTC")!
let day: Int64 = 86_400_000

func timing() {
    typealias T = NotificationsTiming
    eq("after first", T.firstFire(.after(500), anchorMs: 1_000, local: utc), 1_500)
    eq("after once", T.nextAfter(.after(500), anchorMs: 1_000, afterMs: 1_500, local: utc), nil)
    eq("at first", T.firstFire(.at(5_000), anchorMs: 1_000, local: utc), 5_000)
    eq("past at at once", T.firstFire(.at(500), anchorMs: 1_000, local: utc), 1_000)
    eq("every first", T.firstFire(.every(60_000), anchorMs: 0, local: utc), 60_000)
    eq("every next", T.nextAfter(.every(60_000), anchorMs: 0, afterMs: 60_000, local: utc), 120_000)
    // Missed several: the next one on the anchor's grid, not a period from now.
    eq("every grid", T.nextAfter(.every(60_000), anchorMs: 0, afterMs: 250_000, local: utc), 300_000)
    let daily = Notifications.Trigger.calendar(rule(0, 0))
    eq("calendar first", T.firstFire(daily, anchorMs: 0, local: utc), day)
    eq("calendar next", T.nextAfter(daily, anchorMs: 0, afterMs: day, local: utc), 2 * day)
    // A New York 02:30 on the spring-forward day is 03:30 (Rust's rule),
    // not UNCalendarNotificationTrigger's 03:00.
    let gap = Notifications.Trigger.calendar(rule(2, 30, zone: "America/New_York"))
    eq("dst gap", T.nextAfter(gap, anchorMs: ms("2026-03-07T12:00:00Z"), afterMs: ms("2026-03-07T12:00:00Z"),
                              local: utc), ms("2026-03-08T07:30:00Z"))

    eq("on time", T.delivery(.drop, latestMs: 0, nowMs: T.lateAfterMs), false)
    eq("dropped", T.delivery(.drop, latestMs: 0, nowMs: T.lateAfterMs + 1), nil)
    eq("late", T.delivery(.deliverLate(3_600_000), latestMs: 0, nowMs: 1_800_000), true)
    eq("too late", T.delivery(.deliverLate(3_600_000), latestMs: 0, nowMs: 7_200_000), nil)
    eq("latest every", T.latestOccurrence(.every(60_000), anchorMs: 0, at: 60_000, nowMs: 250_000, local: utc),
       240_000)
    let nine = Notifications.Trigger.calendar(rule(9, 0))
    eq("latest daily", T.latestOccurrence(nine, anchorMs: 0, at: day + 9 * 3_600_000,
                                          nowMs: 4 * day + 10 * 3_600_000, local: utc), 4 * day + 9 * 3_600_000)
}

func note(_ id: String, title: String = "Title", missed: Notifications.MissedPolicy = .deliverLate(3_600_000),
          style: Notifications.Style = .plain, actions: [Notifications.Action] = [], url: String? = nil,
          body: String? = nil, icon: Notifications.NotificationImage? = nil, userData: String? = nil,
          progress: Notifications.Progress? = nil) -> Notifications.Notification {
    Notifications.Notification(
        id: Notifications.NotificationId(value: id), channel: nil, title: title, subtitle: nil, body: body,
        style: style, icon: icon, actions: actions, url: url, userData: userData, group: nil, category: nil,
        interruption: nil, sound: nil, progress: progress, ongoing: false, live: false, statusText: nil,
        timeoutMs: nil, timestampMs: nil, chronometer: nil, count: nil, lockScreen: nil, hideInForeground: false,
        missed: missed, android: nil, apple: nil, linuxHints: [])
}

func plan() {
    typealias Entry = NotificationsPlan.Entry
    let nine = Notifications.Trigger.calendar(rule(9, 0))
    let at9 = { (d: Int64) in d * day + 9 * 3_600_000 }
    let now = at9(4) + 1_800_000
    // Armed through day 2, missed days 3 and 4: late by 30 min from the
    // latest one, delivered once; then day 5.
    let daily = Entry(notification: note("d"), trigger: nine, anchorMs: 0, nextFireMs: at9(1),
                      armedThroughMs: at9(2))
    let (moved, late) = NotificationsPlan.settle(daily, nowMs: now, local: utc)
    eq("settle late", late, true)
    eq("settle next", moved?.nextFireMs, at9(5))
    let dropped = Entry(notification: note("d", missed: .drop), trigger: nine, anchorMs: 0, nextFireMs: at9(1),
                        armedThroughMs: at9(2))
    let (droppedMoved, droppedLate) = NotificationsPlan.settle(dropped, nowMs: now, local: utc)
    eq("settle drop", droppedLate, nil)
    eq("settle drop next", droppedMoved?.nextFireMs, at9(5))
    // Armed past now: the system delivers; still-armed ones stay next.
    let ahead = Entry(notification: note("d"), trigger: nine, anchorMs: 0, nextFireMs: at9(1),
                      armedThroughMs: at9(10))
    let (aheadMoved, aheadLate) = NotificationsPlan.settle(ahead, nowMs: now, local: utc)
    eq("settle armed", aheadLate, nil)
    eq("settle armed next", aheadMoved?.nextFireMs, at9(5))
    // A one-shot never armed, 30 s overdue: on time, then gone.
    let shot = Entry(notification: note("s"), trigger: .after(1_000), anchorMs: 0, nextFireMs: 1_000,
                     armedThroughMs: nil)
    let (shotMoved, shotLate) = NotificationsPlan.settle(shot, nowMs: 31_000, local: utc)
    eq("one-shot on time", shotLate, false)
    check("one-shot gone", shotMoved == nil)
    // Armed and past: the system delivered it.
    let armedShot = Entry(notification: note("s"), trigger: .after(1_000), anchorMs: 0, nextFireMs: 1_000,
                          armedThroughMs: 1_000)
    let (armedMoved, armedLate) = NotificationsPlan.settle(armedShot, nowMs: 600_000, local: utc)
    eq("armed one-shot", armedLate, nil)
    check("armed one-shot gone", armedMoved == nil)

    let every = Entry(notification: note("e1"), trigger: .every(60_000), anchorMs: 0, nextFireMs: 60_000,
                      armedThroughMs: nil)
    let once = Entry(notification: note("e2"), trigger: .at(90_000), anchorMs: 0, nextFireMs: 90_000,
                     armedThroughMs: nil)
    let wanted = NotificationsPlan.soonest([once, every], limit: 3, local: utc)
    eq("soonest ids", wanted.map { $0.requestId }, ["e1@60000", "e2", "e1@120000"])
    eq("soonest fires", wanted.map { $0.fireMs }, [60_000, 90_000, 120_000])
    eq("no slots", NotificationsPlan.soonest([every], limit: 0, local: utc).count, 0)
    eq("cap", NotificationsPlan.soonest([every], limit: 60, local: utc).count, 60)
}

func problem(_ body: () throws -> Void) -> String {
    do {
        try body()
        return "ok"
    } catch Notifications.NotificationError.invalid(let field) {
        return "\(field.field) \(field.problem)"
    } catch {
        return "other \(error)"
    }
}

func button(_ id: String, _ title: String = "T", kind: Notifications.ActionKind = .button) -> Notifications.Action {
    Notifications.Action(id: Notifications.ActionId(value: id), title: title, kind: kind, opensApp: true,
                         destructive: false, requiresUnlock: false, url: nil, icon: nil)
}

func patch(style: Notifications.Style? = nil, actions: [Notifications.Action]? = nil,
           progress: Notifications.Progress? = nil, clear: [Notifications.PatchField] = []) -> Notifications.NotificationPatch {
    Notifications.NotificationPatch(title: nil, subtitle: nil, body: nil, style: style, icon: nil, actions: actions,
                                    progress: progress, statusText: nil, count: nil, ongoing: nil, url: nil,
                                    userData: nil, clear: clear, alertAgain: false)
}

func validation() {
    typealias V = NotificationsValidate
    eq("plain", problem { try V.check(note("a")) }, "ok")
    eq("blank title", problem { try V.check(note("a", title: "  ")) }, "title empty")
    eq("control title", problem { try V.check(note("a", title: "a\nb")) }, "title badCharset")
    eq("multiline body", problem { try V.check(note("a", body: "line 1\nline 2")) }, "ok")
    eq("long body", problem { try V.check(note("a", body: String(repeating: "x", count: V.maxBodyChars + 1))) },
       "body tooLong")
    eq("bad id", problem { try V.check(note("a b")) }, "id badCharset")
    eq("duplicate action", problem { try V.check(note("a", actions: [button("a"), button("a")])) },
       "actions[1].id duplicate")
    eq("reserved action", problem { try V.check(note("a", actions: [button("istmo.call.answer")])) },
       "actions[0].id reserved")
    let caller = Notifications.Participant(key: "ana", name: "Ana", avatar: nil, isBot: false, important: false)
    let call = Notifications.Style.call(Notifications.CallInfo(caller: caller, kind: .incoming, video: false))
    eq("call action", problem { try V.check(note("a", style: call, actions: [button("istmo.call.answer")])) }, "ok")
    eq("empty select", problem { try V.check(note("a", actions: [button("s", kind: .select([]))])) },
       "actions[0].choices empty")
    eq("short snooze", problem { try V.check(note("a", actions: [button("z", kind: .snooze(10))])) },
       "actions[0].snooze outOfRange")
    eq("url", problem { try V.check(note("a", url: "app://files/42")) }, "ok")
    eq("mailto", problem { try V.check(note("a", url: "mailto:a@b.c")) }, "ok")
    for bad in ["files/42", "1app://x", "app://a b", ":x"] {
        eq("bad url \(bad)", problem { try V.check(note("a", url: bad)) }, "url badUrl")
    }
    eq("big icon", problem { try V.check(note("a", icon: .png(Data(count: V.maxImageBytes + 1)))) },
       "icon tooLarge")
    eq("big user data", problem {
        try V.check(note("a", userData: String(repeating: "x", count: V.maxUserDataLen + 1)))
    }, "user_data tooLarge")
    eq("relative icon", problem { try V.check(note("a", icon: .file("icon.png"))) }, "icon notAllowedHere")
    let bar = { (v: Float?) in Notifications.Progress(value: v, label: nil, segments: [], points: []) }
    eq("progress", problem { try V.check(note("a", progress: bar(1.5))) }, "progress.value outOfRange")
    eq("nan progress", problem { try V.check(note("a", progress: bar(.nan))) }, "progress.value outOfRange")
    eq("indeterminate", problem { try V.check(note("a", progress: bar(nil))) }, "ok")

    eq("patch", problem { try V.check(patch(progress: bar(0.2))) }, "ok")
    eq("patch set and clear", problem { try V.check(patch(progress: bar(0.2), clear: [.progress])) },
       "progress notAllowedHere")
    eq("patch call actions", problem { try V.check(patch(actions: [button("istmo.call.answer")])) }, "ok")
    eq("patch plain call actions", problem {
        try V.check(patch(style: .plain, actions: [button("istmo.call.answer")]))
    }, "actions[0].id reserved")

    eq("every", problem { try V.check(Notifications.Trigger.every(V.minRepeatMs)) }, "ok")
    eq("short every", problem { try V.check(Notifications.Trigger.every(1_000)) }, "trigger.every outOfRange")
    eq("hour", problem { try V.check(Notifications.Trigger.calendar(rule(24, 0))) }, "trigger.hour outOfRange")
    eq("feb 30", problem { try V.check(Notifications.Trigger.calendar(rule(9, 0, day: 30, month: 2))) },
       "trigger.day_of_month outOfRange")
    eq("feb 29", problem { try V.check(Notifications.Trigger.calendar(rule(9, 0, day: 29, month: 2))) }, "ok")
    eq("weekdays", problem { try V.check(Notifications.Trigger.calendar(rule(9, 0, weekdays: [.mon, .mon]))) },
       "trigger.weekdays duplicate")
    eq("zone", problem { try V.check(Notifications.Trigger.calendar(rule(9, 0, zone: "Mars/Olympus"))) },
       "trigger.time_zone outOfRange")
    eq("berlin", problem { try V.check(Notifications.Trigger.calendar(rule(9, 0, zone: "Europe/Berlin"))) }, "ok")

    let channel = Notifications.ChannelSpec(
        id: Notifications.ChannelId(value: "c"), name: " ", description: nil, importance: .normal, sound: nil,
        vibration: nil, lightColor: nil, showBadge: true, lockScreen: nil, bypassDnd: false, group: nil)
    eq("channel name", problem { try V.check(channel, index: 2) }, "channels[2].name empty")
}

let args = CommandLine.arguments
guard args.count == 2 else { fatalError("usage: checks <schedule_vectors.tsv>") }
vectors(args[1])
timing()
plan()
validation()
if failures.isEmpty {
    print("istmo.notifications iOS logic checks pass")
} else {
    for failure in failures { print("FAIL \(failure)") }
    exit(1)
}
