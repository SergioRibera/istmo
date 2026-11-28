// JVM checks of the plain-Kotlin parts of the Android backend: the
// calendar rule against `tests/schedule_vectors.tsv`, the coalescer
// against the unit cases of `src/coalesce.rs`, trigger timing, missed
// deliveries, the store round trip and the validation rules of
// `src/validate.rs` and the deep links of responses (`src/responses.rs`).
// Run by `check.sh`.
import dev.istmo.plugins.notifications.*
import dev.istmo.plugins.notifications.CalendarRule
import dev.istmo.plugins.notifications.CalendarRules
import dev.istmo.plugins.notifications.Coalescer
import dev.istmo.plugins.notifications.Coalescer.Admit
import dev.istmo.plugins.notifications.Coalescer.Urgency
import dev.istmo.plugins.notifications.MissedPolicy
import dev.istmo.plugins.notifications.Notification
import dev.istmo.plugins.notifications.NotificationId
import dev.istmo.plugins.notifications.NotificationsCodecsImpl
import dev.istmo.plugins.notifications.ScheduleEngine
import dev.istmo.plugins.notifications.ScheduleEntry
import dev.istmo.plugins.notifications.ScheduleStore
import dev.istmo.plugins.notifications.Style
import dev.istmo.plugins.notifications.Trigger
import dev.istmo.plugins.notifications.Triggers
import dev.istmo.plugins.notifications.Weekday
import dev.istmo.runtime.Bincode
import java.io.File
import java.time.Instant
import java.util.TimeZone
import kotlin.system.exitProcess

private val failures = ArrayList<String>()

private fun check(name: String, ok: Boolean, detail: () -> String = { "" }) {
    if (!ok) failures.add("$name ${detail()}")
}

private fun <T> eq(name: String, got: T, want: T) = check(name, got == want) { "expected $want, got $got" }

private fun ms(text: String): Long = Instant.parse(text).toEpochMilli()

private fun fmt(list: List<Long>): String =
    if (list.isEmpty()) "none" else list.joinToString(",") { Instant.ofEpochMilli(it).toString() }

private fun weekday(text: String): Weekday = when (text) {
    "mon" -> Weekday.Mon
    "tue" -> Weekday.Tue
    "wed" -> Weekday.Wed
    "thu" -> Weekday.Thu
    "fri" -> Weekday.Fri
    "sat" -> Weekday.Sat
    "sun" -> Weekday.Sun
    else -> error("bad weekday $text")
}

private fun opt(text: String): String? = if (text == "-") null else text

private fun vectors(path: String) {
    var rows = 0
    for (line in File(path).readLines()) {
        if (line.startsWith("#") || line.isBlank() || line.startsWith("name\t")) continue
        val c = line.split('\t')
        check("row", c.size == 12) { line }
        if (c.size != 12) continue
        val rule = CalendarRule(
            hour = c[3].toUByte(),
            minute = c[4].toUByte(),
            weekdays = opt(c[5])?.split(',')?.map(::weekday) ?: emptyList(),
            dayOfMonth = opt(c[6])?.toUByte(),
            month = opt(c[7])?.toUByte(),
            timeZone = opt(c[1]),
            untilMs = opt(c[8])?.let(::ms),
        )
        val local = TimeZone.getTimeZone(c[2])
        val got = fmt(CalendarRules.nextOccurrences(rule, ms(c[9]), c[10].toInt(), local))
        eq("vector ${c[0]}", got, c[11])
        rows++
    }
    check("vector rows", rows >= 30) { "only $rows" }
    println("schedule vectors: $rows rows")
}

private fun coalescer() {
    run {
        val c = Coalescer<Int>(2, 2, 0)
        eq("burst 1", c.offer("a", 1, Urgency.REGULAR, 0), Admit.Now(1))
        eq("burst 2", c.offer("a", 2, Urgency.REGULAR, 0), Admit.Now(2))
        eq("burst 3", c.offer("a", 3, Urgency.REGULAR, 0), Admit.Queued)
        eq("burst 4", c.offer("a", 4, Urgency.REGULAR, 10), Admit.Queued)
        eq("burst queued", c.queued(), 1)
        eq("burst due", c.nextDue(), 500L)
        eq("burst poll 499", c.poll(499), emptyList())
        eq("burst poll 500", c.poll(500), listOf("a" to 4))
        eq("burst due none", c.nextDue(), null)
    }
    run {
        val c = Coalescer<Int>(1, 1, 0)
        eq("order 1", c.offer("a", 1, Urgency.REGULAR, 0), Admit.Now(1))
        eq("order 2", c.offer("b", 10, Urgency.REGULAR, 100), Admit.Queued)
        eq("order 3", c.offer("a", 2, Urgency.REGULAR, 200), Admit.Queued)
        eq("order poll 1", c.poll(1_000), listOf("b" to 10))
        eq("order 4", c.offer("b", 11, Urgency.REGULAR, 1_500), Admit.Queued)
        eq("order poll 2", c.poll(2_000), listOf("a" to 2))
        eq("order poll 3", c.poll(3_000), listOf("b" to 11))
    }
    run {
        val c = Coalescer<Int>(1, 1, 0)
        eq("jump 1", c.offer("a", 1, Urgency.REGULAR, 0), Admit.Now(1))
        eq("jump 2", c.offer("a", 2, Urgency.REGULAR, 0), Admit.Queued)
        eq("jump finish", c.offer("a", 3, Urgency.FINISH, 0), Admit.Now(3))
        eq("jump queued", c.queued(), 0)
        eq("jump 4", c.offer("a", 4, Urgency.REGULAR, 0), Admit.Queued)
        eq("jump due", c.nextDue(), 2_000L)
        eq("jump poll 1999", c.poll(1_999), emptyList())
        eq("jump poll 2000", c.poll(2_000), listOf("a" to 4))
        eq("jump alert", c.offer("a", 5, Urgency.ALERT, 2_000), Admit.Now(5))
    }
    run {
        val c = Coalescer<Int>(1, 1, 0)
        c.offer("a", 1, Urgency.REGULAR, 0)
        eq("cancel queued", c.offer("a", 2, Urgency.REGULAR, 0), Admit.Queued)
        c.cancel("a")
        eq("cancel empty", c.queued(), 0)
        eq("cancel poll", c.poll(10_000), emptyList())
    }
    run {
        val c = Coalescer<Int>(10, 10, 0)
        c.offer("a", 1, Urgency.REGULAR, 0)
        c.dismissed("a")
        check("dismissed flag", c.isDismissed("a"))
        eq("dismissed drop", c.offer("a", 2, Urgency.REGULAR, 10), Admit.Dropped)
        eq("dismissed alert", c.offer("a", 3, Urgency.ALERT, 20), Admit.Now(3))
        check("dismissed cleared", !c.isDismissed("a"))
        eq("dismissed regular", c.offer("a", 4, Urgency.REGULAR, 30), Admit.Now(4))
        c.dismissed("a")
        eq("dismissed finish", c.offer("a", 5, Urgency.FINISH, 40), Admit.Now(5))
        c.dismissed("a")
        c.shown("a", 50)
        eq("dismissed shown", c.offer("a", 6, Urgency.REGULAR, 60), Admit.Now(6))
    }
    run {
        val c = Coalescer<Int>(4, 4, 0)
        for (v in 0 until 4) eq("refill $v", c.offer("a", v, Urgency.REGULAR, 0), Admit.Now(v))
        eq("refill queued", c.offer("a", 9, Urgency.REGULAR, 0), Admit.Queued)
        eq("refill poll", c.poll(10_000), listOf("a" to 9))
        for (v in 0 until 3) eq("refill again $v", c.offer("a", v, Urgency.REGULAR, 10_000), Admit.Now(v))
        eq("refill queued 2", c.offer("a", 7, Urgency.REGULAR, 10_000), Admit.Queued)
        eq("clock back", c.poll(5_000), emptyList())
        eq("refill due", c.nextDue(), 10_250L)
    }
    run {
        // Debt is floored at one burst: a run of alerts never stalls
        // regular updates for longer than refilling two bursts.
        val c = Coalescer<Int>(4, 4, 0)
        for (v in 0 until 100) c.offer("chat$v", v, Urgency.ALERT, 0)
        eq("floor queued", c.offer("dl", 1, Urgency.REGULAR, 0), Admit.Queued)
        eq("floor due", c.nextDue(), 1_250L)
    }
    run {
        val c = Coalescer<Int>(2, 2, 0)
        for (v in 0 until 100) {
            eq("floor2 alert $v", c.offer("a", v, Urgency.ALERT, 0), Admit.Now(v))
            c.shown("a", 0)
        }
        eq("floor2 queued", c.offer("a", 7, Urgency.REGULAR, 0), Admit.Queued)
        eq("floor2 due", c.nextDue(), 1_500L)
        eq("floor2 poll", c.poll(1_500), listOf("a" to 7))
    }
    run {
        val c = Coalescer<Int>(10, 10, 0)
        for (v in 0 until 10) eq("rate $v", c.offer("a", v, Urgency.REGULAR, 0), Admit.Now(v))
        eq("rate queued", c.offer("a", 10, Urgency.REGULAR, 0), Admit.Queued)
        c.setRate(1, 1, 0)
        eq("rate set", c.perSecond to c.burst, 1 to 1)
        eq("rate kept", c.queued(), 1)
        eq("rate due", c.nextDue(), 1_000L)
        eq("rate poll", c.poll(1_000), listOf("a" to 10))
    }
    println("coalescer cases: done")
}

private fun notification(id: String, missed: MissedPolicy = MissedPolicy.DeliverLate(3_600_000u)) = Notification(
    id = NotificationId(id), channel = null, title = "t", subtitle = null, body = null, style = Style.Plain,
    icon = null, actions = emptyList(), url = null, userData = null, group = null, category = null,
    interruption = null, sound = null, progress = null, ongoing = false, live = false, statusText = null,
    timeoutMs = null, timestampMs = null, chronometer = null, count = null, lockScreen = null,
    hideInForeground = false, missed = missed, android = null, apple = null, linuxHints = emptyList(),
)

private fun schedule() {
    eq("after", Triggers.firstFire(Trigger.After(500u), 1_000), 1_500L)
    eq("after once", Triggers.nextAfter(Trigger.After(500u), 1_000, 1_500), null)
    eq("at", Triggers.firstFire(Trigger.At(5_000), 1_000), 5_000L)
    eq("at past", Triggers.firstFire(Trigger.At(500), 1_000), 1_000L)
    val every = Trigger.Every(60_000u)
    eq("every first", Triggers.firstFire(every, 0), 60_000L)
    eq("every next", Triggers.nextAfter(every, 0, 60_000), 120_000L)
    eq("every grid", Triggers.nextAfter(every, 0, 250_000), 300_000L)
    val utc = TimeZone.getTimeZone("UTC")
    val daily = Trigger.Calendar(CalendarRule(0u, 0u, emptyList(), null, null, "UTC", null))
    eq("calendar first", Triggers.firstFire(daily, 0, utc), 86_400_000L)

    // A daily reminder missed for three days is as late as today's.
    val engine = ScheduleEngine(local = { utc })
    eq("add", engine.add(notification("d"), daily, 0), 86_400_000L)
    eq("add never", engine.add(notification("x"), Trigger.Calendar(
        CalendarRule(9u, 0u, emptyList(), 31u, 4u, "UTC", null)), 0), null)
    check("never not stored", !engine.contains("x"))
    val now = 3 * 86_400_000L + 30_000
    val due = engine.due(now)
    eq("late within", due.map { it.notification.id.value to it.late }, listOf("d" to false))
    eq("moved", engine.get("d")?.nextFireMs, 4 * 86_400_000L)
    val drop = ScheduleEngine(local = { utc })
    drop.add(notification("o", MissedPolicy.Drop), Trigger.After(1_000u), 0)
    eq("dropped", drop.due(1_000 + 120_000), emptyList())
    check("dropped gone", drop.isEmpty())
    val late = ScheduleEngine(local = { utc })
    late.add(notification("l"), Trigger.After(1_000u), 0)
    eq("late", late.due(1_000 + 120_000).map { it.late }, listOf(true))


    // `engine.rs` tests: the daily reminder missed three days rejoins its rule,
    // twenty years asleep still counts from the latest occurrence, and a
    // per-minute repeat woken 30 s after an occurrence is on time.
    val nine = Trigger.Calendar(CalendarRule(9u, 0u, emptyList(), null, null, "UTC", null))
    val asleep = ScheduleEngine(local = { utc })
    asleep.add(notification("daily"), nine, 0)
    val woken = 3 * 86_400_000L + 9 * 3_600_000L + 1_800_000L
    eq("three days late", asleep.due(woken).map { it.late }, listOf(true))
    eq("three days rejoin", asleep.nextDeadline(), 4 * 86_400_000L + 9 * 3_600_000L)
    val decades = ScheduleEngine(local = { utc })
    decades.add(notification("daily"), nine, 0)
    val day = 20L * 366
    eq("twenty years", decades.due(day * 86_400_000L + 9 * 3_600_000L + 1_800_000L).map { it.late }, listOf(true))
    val minutes = ScheduleEngine(local = { utc })
    minutes.add(notification("m", MissedPolicy.Drop), every, 0)
    eq("per minute three days", minutes.due(3 * 86_400_000L + 30_000).map { it.late }, listOf(false))
    eq("per minute next", minutes.nextDeadline(), 3 * 86_400_000L + 60_000)
    // Dead entries read from a store are dropped (`Engine::with_entries`).
    val ended = Trigger.Calendar(CalendarRule(9u, 0u, emptyList(), null, null, "UTC", 1_000))
    check("dead dropped", ScheduleEngine(listOf(ScheduleEntry(notification("dead"), ended, 0, null)), { utc }).isEmpty())
    // Store round trip.
    val dir = File(System.getProperty("java.io.tmpdir"), "istmo-notif-check-${System.nanoTime()}")
    val store = ScheduleStore(File(dir, "schedule.bin"), NotificationsCodecsImpl())
    val entries = listOf(ScheduleEntry(notification("s"), every, 7, 60_007), ScheduleEntry(notification("t"), daily, -3, null))
    store.save(entries)
    eq("store", store.load(), entries)
    eq("store format", File(dir, "schedule.bin").readBytes()[0].toInt(), ScheduleStore.STORE_FORMAT)
    store.save(emptyList())
    check("store removed", !File(dir, "schedule.bin").exists())
    dir.deleteRecursively()
    println("schedule engine and store: done")
}


private fun invalid(got: InvalidField?, field: String, problem: Problem) = eq("invalid $field", got, InvalidField(field, problem))

private fun action(id: String, title: String, kind: ActionKind = ActionKind.Button) =
    Action(ActionId(id), title, kind, opensApp = false, destructive = false, requiresUnlock = false, url = null, icon = null)

private fun patch(progress: Progress? = null, actions: List<Action>? = null, style: Style? = null, clear: List<PatchField> = emptyList()) =
    NotificationPatch(null, null, null, style, null, actions, progress, null, null, null, null, null, clear, false)

private fun bar(value: Float?) = Progress(value, null, emptyList(), emptyList())

private fun rule(hour: Int, weekdays: List<Weekday> = emptyList(), day: Int? = null, month: Int? = null, zone: String? = null) =
    CalendarRule(hour.toUByte(), 0u, weekdays, day?.toUByte(), month?.toUByte(), zone, null)

/** The cases of `src/validate.rs`'s tests, same fields and problems. */
private fun validate() {
    val n = notification("a").copy(title = "Title")
    eq("valid", Validate.notification(n), null)
    invalid(Validate.notification(n.copy(title = "  ")), "title", Problem.Empty)
    invalid(Validate.notification(n.copy(title = "a\nb")), "title", Problem.BadCharset)
    eq("multiline body", Validate.notification(n.copy(body = "line 1\nline 2")), null)
    invalid(Validate.notification(n.copy(body = "x".repeat(Validate.MAX_BODY_CHARS + 1))), "body", Problem.TooLong)
    // Characters, not UTF-16 units: 256 emoji are 512 units and still fit.
    eq("title chars", Validate.notification(n.copy(title = "😀".repeat(Validate.MAX_TITLE_CHARS))), null)

    invalid(Validate.notification(n.copy(actions = listOf(action("a", "A"), action("a", "B")))), "actions[1].id", Problem.Duplicate)
    val answer = action("istmo.call.answer", "Answer")
    invalid(Validate.notification(n.copy(actions = listOf(answer))), "actions[0].id", Problem.Reserved)
    val call = Style.Call(CallInfo(Participant("ana", "Ana", null, isBot = false, important = false), CallKind.Incoming, false))
    eq("call actions", Validate.notification(n.copy(style = call, actions = listOf(answer))), null)
    invalid(Validate.notification(n.copy(actions = listOf(action("s", "Pick", ActionKind.Select(emptyList()))))), "actions[0].choices", Problem.Empty)
    invalid(Validate.notification(n.copy(actions = listOf(action("z", "Later", ActionKind.Snooze(10u))))), "actions[0].snooze", Problem.OutOfRange)

    eq("url scheme", Validate.notification(n.copy(url = "app://files/42")), null)
    eq("url mailto", Validate.notification(n.copy(url = "mailto:a@b.c")), null)
    for (bad in listOf("files/42", "1app://x", "app://a b", ":x")) invalid(Validate.notification(n.copy(url = bad)), "url", Problem.BadUrl)

    invalid(Validate.notification(n.copy(icon = NotificationImage.Png(ByteArray(Validate.MAX_IMAGE_BYTES + 1)))), "icon", Problem.TooLarge)
    invalid(Validate.notification(n.copy(userData = "x".repeat(Validate.MAX_USER_DATA_LEN + 1))), "user_data", Problem.TooLarge)
    invalid(Validate.notification(n.copy(icon = NotificationImage.File("icon.png"))), "icon", Problem.NotAllowedHere)

    invalid(Validate.notification(n.copy(progress = bar(1.5f))), "progress.value", Problem.OutOfRange)
    check("nan bar", Validate.notification(n.copy(progress = bar(Float.NaN))) != null)
    eq("indeterminate", Validate.notification(n.copy(progress = bar(null))), null)
    invalid(Validate.notification(n.copy(timeoutMs = 0u)), "timeout_ms", Problem.OutOfRange)
    invalid(Validate.notification(n.copy(id = NotificationId("a b"))), "id", Problem.BadCharset)

    eq("patch", Validate.patch(patch(progress = bar(0.2f))), null)
    invalid(Validate.patch(patch(progress = bar(0.2f), clear = listOf(PatchField.Progress))), "progress", Problem.NotAllowedHere)
    eq("patch clear", Validate.patch(patch(clear = listOf(PatchField.Progress))), null)
    eq("patch call actions", Validate.patch(patch(actions = listOf(answer))), null)
    invalid(Validate.patch(patch(actions = listOf(answer), style = Style.Plain)), "actions[0].id", Problem.Reserved)

    eq("every min", Validate.trigger(Trigger.Every(Validate.MIN_REPEAT_MS)), null)
    invalid(Validate.trigger(Trigger.Every(1_000u)), "trigger.every", Problem.OutOfRange)
    eq("daily", Validate.trigger(Trigger.Calendar(CalendarRule(23u, 59u, emptyList(), null, null, null, null))), null)
    invalid(Validate.trigger(Trigger.Calendar(rule(24))), "trigger.hour", Problem.OutOfRange)
    invalid(Validate.rule(rule(9, day = 30, month = 2)), "trigger.day_of_month", Problem.OutOfRange)
    eq("feb 29", Validate.rule(rule(9, day = 29, month = 2)), null)
    invalid(Validate.rule(rule(9, weekdays = listOf(Weekday.Mon, Weekday.Mon))), "trigger.weekdays", Problem.Duplicate)
    invalid(Validate.rule(rule(9, zone = "Mars/Olympus")), "trigger.time_zone", Problem.OutOfRange)
    eq("berlin", Validate.rule(rule(9, zone = "Europe/Berlin")), null)

    val channel = ChannelSpec(ChannelId("c"), "Chats", null, Interruption.Normal, null, null, null, true, null, false, null)
    eq("channel", Validate.channels(listOf(channel)), null)
    invalid(Validate.channels(listOf(channel, channel.copy(name = ""))), "channels[1].name", Problem.Empty)
    invalid(Validate.channels(listOf(channel.copy(vibration = List(33) { 1u }))), "channels[0].vibration", Problem.TooLong)
    println("validation cases: done")
}

private fun response(action: ResponseAction, url: String?) =
    NotificationResponse(NotificationId("n"), action, null, false, null, url, false, emptyList(), null)

/** `only_opening_responses_publish_their_url` in `src/responses.rs`. */
private fun deepLinks() {
    for ((name, action, source) in listOf(
        Triple("open", ResponseAction.Open, "notification:n"),
        Triple("button", ResponseAction.Button(ActionId("go")), "notification:n:go"),
    )) {
        val bytes = ResponseDeepLink.encode(response(action, "app://x"), 5)
        check("deep link $name", bytes != null)
        if (bytes == null) continue
        val uri = Bincode.readString(bytes, 0)
        eq("deep link $name uri", uri.value, "app://x")
        eq("deep link $name source tag", bytes[uri.consumed], 1.toByte())
        val src = Bincode.readString(bytes, uri.consumed + 1)
        eq("deep link $name source", src.value, source)
        eq("deep link $name received tag", bytes[src.consumed], 1.toByte())
        eq("deep link $name received", Bincode.readVarintU64(bytes, src.consumed + 1).value, 5L)
    }
    for (action in listOf(ResponseAction.Dismiss, ResponseAction.More)) {
        eq("deep link $action", ResponseDeepLink.encode(response(action, "app://x"), 5), null)
    }
    eq("deep link without url", ResponseDeepLink.encode(response(ResponseAction.Open, null), 5), null)
    eq("deep link empty url", ResponseDeepLink.encode(response(ResponseAction.Open, ""), 5), null)
    println("deep link cases: done")
}

fun main(args: Array<String>) {
    vectors(args[0])
    coalescer()
    schedule()
    validate()
    deepLinks()
    if (failures.isNotEmpty()) {
        failures.forEach(System.err::println)
        exitProcess(1)
    }
    println("android native checks pass")
}
