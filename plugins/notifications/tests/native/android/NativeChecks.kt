// JVM checks of the plain-Kotlin parts of the Android backend: the
// calendar rule against `tests/schedule_vectors.tsv`, the coalescer
// against the unit cases of `src/coalesce.rs`, trigger timing, missed
// deliveries and the store round trip. Run by `check.sh`.
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

fun main(args: Array<String>) {
    vectors(args[0])
    coalescer()
    schedule()
    if (failures.isNotEmpty()) {
        failures.forEach(System.err::println)
        exitProcess(1)
    }
    println("android native checks pass")
}
