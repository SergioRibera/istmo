package dev.istmo.plugins.notifications

/**
 * Live-update coalescing, the Kotlin port of `src/coalesce.rs` (the
 * reference; its unit tests are ported in `tests/native/android`).
 *
 * - Per id the latest pending update wins; older ones are never posted.
 * - An integer milli-token bucket (`perSecond`, `burst`) limits posts per
 *   app: refill `elapsedMs * perSecond` milli-tokens, cap `burst * 1000`.
 * - [Urgency.ALERT] and [Urgency.FINISH] post at once and spend a token;
 *   the debt they leave is floored at `-burst * 1000`.
 * - A regular update for a notification the user dismissed is dropped.
 *
 * Deterministic and not thread-safe: time comes in as milliseconds, the
 * caller serialises access.
 */
class Coalescer<T>(perSecond: Int, burst: Int, nowMs: Long) {
    enum class Urgency { REGULAR, ALERT, FINISH }

    /** What to do with an offered update. */
    sealed class Admit<out T> {
        data class Now<T>(val item: T) : Admit<T>()
        object Queued : Admit<Nothing>()
        object Dropped : Admit<Nothing>()
    }

    val perSecond: Int = if (perSecond <= 0) 1 else perSecond
    val burst: Int = if (burst <= 0) 1 else burst

    private var tokens: Long = this.burst.toLong() * TOKEN
    private var refilledMs: Long = nowMs
    private val order = ArrayDeque<String>()
    private val pending = HashMap<String, T>()
    private val dismissed = HashSet<String>()

    private fun refill(nowMs: Long) {
        val elapsed = maxOf(0L, nowMs - refilledMs)
        refilledMs = maxOf(refilledMs, nowMs)
        val full = burst.toLong() * TOKEN
        tokens = minOf(full, tokens + elapsed * perSecond)
    }

    private fun take(): Boolean {
        if (tokens < TOKEN) return false
        tokens -= TOKEN
        return true
    }

    /** Spends a token even when there is none, down to the debt floor. */
    private fun spend() {
        tokens = maxOf(-burst.toLong() * TOKEN, tokens - TOKEN)
    }

    private fun unqueue(id: String): T? {
        if (!pending.containsKey(id)) return null
        val item = pending.remove(id)
        order.remove(id)
        return item
    }

    /** An update for [id] at [nowMs]. */
    fun offer(id: String, item: T, urgency: Urgency, nowMs: Long): Admit<T> {
        refill(nowMs)
        return when (urgency) {
            Urgency.REGULAR -> {
                if (id in dismissed) return Admit.Dropped
                if (pending.containsKey(id)) {
                    pending[id] = item
                    return Admit.Queued
                }
                // Queued updates of other ids go first.
                if (order.isEmpty() && take()) return Admit.Now(item)
                order.addLast(id)
                pending[id] = item
                Admit.Queued
            }
            Urgency.ALERT, Urgency.FINISH -> {
                dismissed.remove(id)
                unqueue(id)
                spend()
                Admit.Now(item)
            }
        }
    }

    /** A full show of [id]: forgets its dismissal and queued update, spends a token. */
    fun shown(id: String, nowMs: Long) {
        refill(nowMs)
        dismissed.remove(id)
        unqueue(id)
        spend()
    }

    /** The user dismissed [id]: its queued update and later regular ones are dropped. */
    fun dismissed(id: String) {
        unqueue(id)
        dismissed.add(id)
    }

    /** [id] is gone for good: drops its queued update and its dismissal. */
    fun cancel(id: String) {
        unqueue(id)
        dismissed.remove(id)
    }

    fun clear() {
        order.clear()
        pending.clear()
        dismissed.clear()
    }

    /** The queued updates that may go at [nowMs], oldest first. */
    fun poll(nowMs: Long): List<Pair<String, T>> {
        refill(nowMs)
        val out = ArrayList<Pair<String, T>>()
        while (order.isNotEmpty() && take()) {
            val id = order.removeFirst()
            @Suppress("UNCHECKED_CAST")
            if (pending.containsKey(id)) out.add(id to (pending.remove(id) as T))
        }
        return out
    }

    /** When the next queued update may go, `null` when nothing is queued. */
    fun nextDue(): Long? {
        if (order.isEmpty()) return null
        val missing = maxOf(0L, TOKEN - tokens)
        val perMs = perSecond.toLong()
        // Round up: a token is whole only after the full wait.
        return refilledMs + (missing + perMs - 1) / perMs
    }

    fun queued(): Int = order.size

    fun isDismissed(id: String): Boolean = id in dismissed

    companion object {
        /** Milli-tokens per token. */
        const val TOKEN = 1_000L

        /** `Rate::ANDROID`: 4 per second, burst 4. */
        const val ANDROID_PER_SECOND = 4
        const val ANDROID_BURST = 4
    }
}
