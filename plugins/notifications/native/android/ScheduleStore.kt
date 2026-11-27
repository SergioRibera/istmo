package dev.istmo.plugins.notifications

import dev.istmo.runtime.Bincode
import dev.istmo.runtime.IstmoBytesOut
import java.io.File
import java.io.FileOutputStream
import java.util.TimeZone

/** One scheduled notification (mirrors `schedule::Entry`). */
data class ScheduleEntry(
    val notification: Notification,
    val trigger: Trigger,
    val anchorMs: Long,
    val nextFireMs: Long?,
)

/** A notification whose time came; [late] when delivered within its `DeliverLate` window. */
data class DueNotification(val notification: Notification, val late: Boolean)

/**
 * The scheduled entries, the port of `schedule/engine.rs`: pure, time
 * comes in as arguments. [ScheduleStore] keeps it on disk; the soonest
 * [nextDeadline] is the one `AlarmManager` alarm armed.
 */
class ScheduleEngine(entries: List<ScheduleEntry> = emptyList(), private val local: () -> TimeZone = TimeZone::getDefault) {
    private val entries = LinkedHashMap<String, ScheduleEntry>().apply {
        for (entry in entries) put(entry.notification.id.value, entry)
    }

    /**
     * Schedules [notification] (replacing one with the same id) at [nowMs].
     * `null` when the trigger never fires (nothing is stored).
     */
    fun add(notification: Notification, trigger: Trigger, nowMs: Long): Long? {
        val next = Triggers.firstFire(trigger, nowMs, local()) ?: return null
        entries[notification.id.value] = ScheduleEntry(notification, trigger, nowMs, next)
        return next
    }

    fun remove(id: String): Boolean = entries.remove(id) != null

    /** Removes every entry of [group]; the removed ids. */
    fun removeGroup(group: String): List<String> {
        val ids = entries.values.filter { it.notification.group?.id?.value == group }.map { it.notification.id.value }
        for (id in ids) entries.remove(id)
        return ids
    }

    fun clear() = entries.clear()

    fun contains(id: String): Boolean = entries.containsKey(id)

    fun isEmpty(): Boolean = entries.isEmpty()

    fun get(id: String): ScheduleEntry? = entries[id]

    /** Every entry, soonest first (never-firing last), ties by id. */
    fun entries(): List<ScheduleEntry> =
        entries.values.sortedWith(compareBy<ScheduleEntry>({ it.nextFireMs ?: Long.MAX_VALUE }, { it.notification.id.value }))

    fun nextDeadline(): Long? = entries.values.mapNotNull { it.nextFireMs }.minOrNull()

    /**
     * The clock or the zone changed: every future delivery is computed
     * again from [nowMs] (local-zone calendar rules follow the new zone).
     * Entries already due keep their time, so [due] still applies the
     * missed policy to them.
     */
    fun recompute(nowMs: Long) {
        for ((id, entry) in entries.entries.toList()) {
            val at = entry.nextFireMs ?: continue
            if (at <= nowMs) continue
            val next = Triggers.nextAfter(entry.trigger, entry.anchorMs, nowMs, local())
            if (next == null) entries.remove(id) else entries[id] = entry.copy(nextFireMs = next)
        }
    }

    /**
     * Takes every entry due at [nowMs]: one-shots leave, repeating ones
     * move to their next occurrence after [nowMs]. More than
     * [LATE_AFTER_MS] late follows the entry's `MissedPolicy`; lateness
     * counts from the latest missed occurrence. Deadline order.
     */
    fun due(nowMs: Long): List<DueNotification> {
        val zone = local()
        val ready = entries.values
            .mapNotNull { e -> e.nextFireMs?.takeIf { it <= nowMs }?.let { it to e.notification.id.value } }
            .sortedWith(compareBy({ it.first }, { it.second }))
        val out = ArrayList<DueNotification>(ready.size)
        for ((at, id) in ready) {
            val entry = entries[id] ?: continue
            val lateness = nowMs - latestOccurrence(entry, at, nowMs, zone)
            val deliver: Boolean? = if (lateness <= LATE_AFTER_MS) {
                false
            } else {
                when (val missed = entry.notification.missed) {
                    is MissedPolicy.Drop -> null
                    is MissedPolicy.DeliverLate -> {
                        val max = if (missed.value > Long.MAX_VALUE.toULong()) Long.MAX_VALUE else missed.value.toLong()
                        if (lateness <= max) true else null
                    }
                }
            }
            val next = Triggers.nextAfter(entry.trigger, entry.anchorMs, nowMs, zone)
            if (next != null) entries[id] = entry.copy(nextFireMs = next) else entries.remove(id)
            if (deliver != null) out.add(DueNotification(entry.notification, deliver))
        }
        return out
    }

    private fun latestOccurrence(entry: ScheduleEntry, at: Long, nowMs: Long, zone: TimeZone): Long =
        when (val trigger = entry.trigger) {
            is Trigger.After, is Trigger.At -> at
            is Trigger.Every -> {
                val period = maxOf(1L, if (trigger.value > Long.MAX_VALUE.toULong()) Long.MAX_VALUE else trigger.value.toLong())
                val steps = (nowMs - entry.anchorMs) / period
                maxOf(at, Triggers.saturatingAdd(entry.anchorMs, if (steps != 0L && period > Long.MAX_VALUE / steps) Long.MAX_VALUE else steps * period))
            }
            is Trigger.Calendar -> {
                var last = at
                for (i in 0 until MAX_MISSED_WALK) {
                    val next = Triggers.nextAfter(trigger, entry.anchorMs, last, zone)
                    if (next == null || next > nowMs) break
                    last = next
                }
                last
            }
        }

    companion object {
        /** `schedule::LATE_AFTER_MS`. */
        const val LATE_AFTER_MS = 60_000L
        private const val MAX_MISSED_WALK = 4_096
    }
}

/**
 * The schedule on disk: `noBackupFilesDir/istmo-notifications/schedule.bin`,
 * format byte [STORE_FORMAT] then the wire encoding of
 * `Vec<(Notification, Trigger, i64 anchor_ms, Option<i64> next_fire_ms)>`
 * (the Rust desktop store's layout). Atomic replace through a sibling
 * `.tmp`; an empty schedule removes the file. Plain JVM code.
 */
class ScheduleStore(private val file: File, private val codecs: NotificationsCodecs) {
    fun load(): List<ScheduleEntry> {
        val bytes = try {
            file.readBytes()
        } catch (_: java.io.IOException) {
            return emptyList()
        }
        return try {
            decode(bytes)
        } catch (_: IllegalArgumentException) {
            // A file this version cannot read: start empty rather than crash every alarm.
            emptyList()
        }
    }

    fun save(entries: List<ScheduleEntry>) {
        if (entries.isEmpty()) {
            file.delete()
            return
        }
        file.parentFile?.mkdirs()
        val tmp = File(file.path + ".tmp")
        FileOutputStream(tmp).use { out ->
            out.write(encode(entries))
            out.fd.sync()
        }
        if (!tmp.renameTo(file)) {
            tmp.delete()
            throw java.io.IOException("cannot replace $file")
        }
    }

    fun encode(entries: List<ScheduleEntry>): ByteArray {
        val out = IstmoBytesOut(256)
        out.write(STORE_FORMAT)
        Bincode.writeVec(out, entries) { o, e ->
            codecs.writeNotification(o, e.notification)
            codecs.writeTrigger(o, e.trigger)
            Bincode.writeI64(o, e.anchorMs)
            Bincode.writeOption(o, e.nextFireMs) { p, v -> Bincode.writeI64(p, v) }
        }
        return out.toExactArray()
    }

    fun decode(bytes: ByteArray): List<ScheduleEntry> {
        require(bytes.isNotEmpty() && bytes[0].toInt() == STORE_FORMAT) { "notification store format" }
        return Bincode.readVec(bytes, 1) { b, c ->
            val n = codecs.readNotification(b, c)
            val t = codecs.readTrigger(b, n.consumed)
            val a = Bincode.readI64(b, t.consumed)
            val f = Bincode.readOption(b, a.consumed) { p, o -> Bincode.readI64(p, o) }
            Bincode.Decoded(ScheduleEntry(n.value, t.value, a.value, f.value), f.consumed)
        }.value
    }

    companion object {
        /** `schedule::STORE_FORMAT`. */
        const val STORE_FORMAT = 1
    }
}
