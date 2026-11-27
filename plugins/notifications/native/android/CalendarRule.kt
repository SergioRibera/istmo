package dev.istmo.plugins.notifications

import java.util.TimeZone

/**
 * [CalendarRule] math, the Kotlin port of `src/schedule/rule.rs`, pinned
 * by `tests/schedule_vectors.tsv` (run by `tests/native/android`).
 *
 * A wall-clock time on the days that match every set field. Resolution
 * to an instant follows RFC 5545 / Temporal "compatible": a time in a
 * DST gap moves forward by the gap, a time in an overlap is the
 * **earlier** instant (`java.util.Calendar` would pick the later one, so
 * nothing here goes through it), days that do not exist are skipped,
 * `untilMs` is inclusive, the search is bounded to 401 years.
 *
 * Plain JVM code (no Android types): civil dates are computed by hand,
 * offsets come from [TimeZone.getOffset].
 */
object CalendarRules {
    private const val SEARCH_YEARS = 401
    private const val DAY_MS = 86_400_000L

    /** The rule's zone: its `timeZone` when set, else [local]; `null` for an unknown name. */
    fun zone(rule: CalendarRule, local: TimeZone): TimeZone? {
        val name = rule.timeZone ?: return local
        val zone = TimeZone.getTimeZone(name)
        // Unknown ids come back as GMT.
        return if (zone.id == name) zone else null
    }

    /** Whether [name] is an IANA zone the device knows. */
    fun knownZone(name: String): Boolean = TimeZone.getTimeZone(name).id == name

    /** The first occurrence strictly after [afterMs], `null` when there is none. */
    fun nextAfter(rule: CalendarRule, afterMs: Long, local: TimeZone = TimeZone.getDefault()): Long? {
        val hour = rule.hour.toInt()
        val minute = rule.minute.toInt()
        if (hour > 23 || minute > 59) return null
        val tz = zone(rule, local) ?: return null
        val weekdays = rule.weekdays.map { it.ordinal }.toHashSet()
        val wantMonth = rule.month?.toInt()
        val wantDay = rule.dayOfMonth?.toInt()
        // Start a day early: the previous day's time can still be ahead
        // of `after` around a DST transition at midnight.
        val startDays = (afterMs + tz.getOffset(afterMs)).floorDiv(DAY_MS) - 1
        val start = civil(startDays)
        var year = start[0]
        var month = start[1]
        var firstDay = start[2]
        val lastYear = year + SEARCH_YEARS
        while (year <= lastYear) {
            if (wantMonth == null || wantMonth == month) {
                val len = daysInMonth(year, month)
                val range = when {
                    wantDay == null -> firstDay..len
                    wantDay in firstDay..len -> wantDay..wantDay
                    else -> IntRange.EMPTY
                }
                for (day in range) {
                    val days = daysFromCivil(year, month, day)
                    if (weekdays.isNotEmpty() && weekday(days) !in weekdays) continue
                    val wall = days * DAY_MS + hour * 3_600_000L + minute * 60_000L
                    val at = resolve(tz, wall)
                    if (at <= afterMs) continue
                    val until = rule.untilMs
                    return if (until != null && at > until) null else at
                }
            }
            firstDay = 1
            month += 1
            if (month > 12) {
                month = 1
                year += 1
            }
        }
        return null
    }

    /** The next [count] occurrences after [fromMs]. */
    fun nextOccurrences(rule: CalendarRule, fromMs: Long, count: Int, local: TimeZone = TimeZone.getDefault()): List<Long> {
        val out = ArrayList<Long>(minOf(count, 64))
        var after = fromMs
        while (out.size < count) {
            val next = nextAfter(rule, after, local) ?: break
            out.add(next)
            after = next
        }
        return out
    }

    /**
     * The instant of the local wall time [wall] (local fields read as UTC
     * milliseconds): the earlier one in an overlap, shifted forward by the
     * gap inside a gap.
     */
    internal fun resolve(tz: TimeZone, wall: Long): Long {
        val before = tz.getOffset(wall - DAY_MS)
        val after = tz.getOffset(wall + DAY_MS)
        val early = wall - before
        val late = wall - after
        val earlyValid = tz.getOffset(early) == before
        val lateValid = tz.getOffset(late) == after
        return when {
            earlyValid && lateValid -> minOf(early, late)
            earlyValid -> early
            lateValid -> late
            // A gap: the offset before the transition moves the time forward by the gap.
            else -> early
        }
    }

    /** Monday = 0 for day number [days] since 1970-01-01 (a Thursday). */
    private fun weekday(days: Long): Int = (days + 3).mod(7L).toInt()

    private fun daysInMonth(year: Int, month: Int): Int = when (month) {
        2 -> if ((year % 4 == 0 && year % 100 != 0) || year % 400 == 0) 29 else 28
        4, 6, 9, 11 -> 30
        else -> 31
    }

    /** Days since 1970-01-01 of a proleptic Gregorian date (H. Hinnant). */
    private fun daysFromCivil(year: Int, month: Int, day: Int): Long {
        val y = (if (month <= 2) year - 1 else year).toLong()
        val era = y.floorDiv(400L)
        val yoe = y - era * 400
        val mp = (month + 9) % 12
        val doy = (153 * mp + 2) / 5 + day - 1
        val doe = yoe * 365 + yoe / 4 - yoe / 100 + doy
        return era * 146_097 + doe - 719_468
    }

    /** `[year, month, day]` of day number [days]. */
    private fun civil(days: Long): IntArray {
        val z = days + 719_468
        val era = z.floorDiv(146_097L)
        val doe = z - era * 146_097
        val yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365
        val doy = doe - (365 * yoe + yoe / 4 - yoe / 100)
        val mp = (5 * doy + 2) / 153
        val day = (doy - (153 * mp + 2) / 5 + 1).toInt()
        val month = (if (mp < 10) mp + 3 else mp - 9).toInt()
        val year = (yoe + era * 400 + if (month <= 2) 1 else 0).toInt()
        return intArrayOf(year, month, day)
    }
}

/** Trigger timing, the port of `Trigger::next_after` / `first_fire` (`src/schedule.rs`). */
object Triggers {
    /** First occurrence strictly after [afterMs] of [trigger] armed at [anchorMs]. */
    fun nextAfter(trigger: Trigger, anchorMs: Long, afterMs: Long, local: TimeZone = TimeZone.getDefault()): Long? =
        when (trigger) {
            is Trigger.After -> {
                val at = saturatingAdd(anchorMs, toLong(trigger.value))
                if (at > afterMs) at else null
            }
            is Trigger.At -> if (trigger.value > afterMs) trigger.value else null
            is Trigger.Every -> {
                val period = maxOf(1L, toLong(trigger.value))
                val elapsed = afterMs - anchorMs
                val k = if (elapsed < 0) 1 else elapsed / period + 1
                saturatingAdd(anchorMs, saturatingMul(k, period))
            }
            is Trigger.Calendar -> CalendarRules.nextAfter(trigger.value, maxOf(afterMs, anchorMs), local)
        }

    /** The first delivery of [trigger] armed at [anchorMs]; a past `At` delivers at once. */
    fun firstFire(trigger: Trigger, anchorMs: Long, local: TimeZone = TimeZone.getDefault()): Long? =
        if (trigger is Trigger.At) maxOf(trigger.value, anchorMs) else nextAfter(trigger, anchorMs, anchorMs - 1, local)

    fun repeats(trigger: Trigger): Boolean = trigger is Trigger.Every || trigger is Trigger.Calendar

    private fun toLong(value: ULong): Long = if (value > Long.MAX_VALUE.toULong()) Long.MAX_VALUE else value.toLong()

    internal fun saturatingAdd(a: Long, b: Long): Long {
        val r = a + b
        return if (((a xor r) and (b xor r)) < 0) (if (a < 0) Long.MIN_VALUE else Long.MAX_VALUE) else r
    }

    private fun saturatingMul(a: Long, b: Long): Long {
        // Both are positive here (k >= 1, period >= 1). Math.multiplyExact is API 24.
        return if (a != 0L && b > Long.MAX_VALUE / a) Long.MAX_VALUE else a * b
    }
}
