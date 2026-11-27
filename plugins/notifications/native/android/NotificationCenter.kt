package dev.istmo.plugins.notifications

import android.Manifest
import android.annotation.SuppressLint
import android.app.NotificationChannel
import android.app.NotificationChannelGroup
import android.app.NotificationManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import android.util.Log
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.Bincode
import dev.istmo.runtime.IstmoBytesOut
import java.io.File
import kotlinx.coroutines.channels.SendChannel
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow

/**
 * Process-wide state of the plugin, shared by [NotificationsBackendImpl]
 * and the receivers (which run without a backend when the process was
 * started by an alarm, a boot or a button).
 *
 * - Shown table: the latest content per id (queued updates included),
 *   needed for patches, messages and group summaries. After a restart the
 *   content comes back from the notification's own extras when it was
 *   small enough to store there ([NotificationRenderer.EXTRA_WIRE]).
 * - Active set: what the plugin believes the system shows (id -> group,
 *   plus the group summaries), read back from `getActiveNotifications`
 *   at start; group summaries and the 50-per-app limit use it.
 * - Per-id lanes: every `notify` / `cancel` of one id (and of one group
 *   summary) runs in that id's lane, one at a time. A post checks, inside
 *   the lane, that its content version is still the table's: a cancel or
 *   a newer content since the post was prepared wins. Scheduled
 *   deliveries carry a ticket ([delivering]) that a cancel revokes.
 * - [Coalescer] at `Rate::ANDROID` (4 per second, burst 4) on a
 *   `HandlerThread`, which also runs the ack timeouts and the promotion
 *   checks. Every post (summaries and reply re-posts included) is charged
 *   to it.
 * - Schedule: [ScheduleStore] in `noBackupFilesDir`, one `AlarmManager`
 *   alarm for the soonest entry ([Alarms]).
 * - Events: subscribers of `events()`, with a backlog of 64 until the
 *   first one.
 *
 * Notifications are posted with tag = id and id [ITEM_ID]; group
 * summaries with tag `istmo.group:<group id>` and id [SUMMARY_ID], so no
 * notification id can name a summary.
 *
 * Lock order: [scheduleLock] before [lock], a lane before [lock]; a lane
 * and [scheduleLock] are never held together. [lock] is held across no
 * binder call or file IO (except once, while [init] loads).
 */
@SuppressLint("MissingPermission") // POST_NOTIFICATIONS is checked before every post.
internal object NotificationCenter {
    private const val TAG = "istmo.notifications"
    const val DEFAULT_CHANNEL = "default"
    const val ITEM_ID = 0
    const val SUMMARY_ID = 1
    const val SUMMARY_PREFIX = "istmo.group:"
    private const val BACKLOG = 64
    private const val ACK_TIMEOUT_MS = 9_000L
    private const val PROMOTION_CHECK_MS = 1_000L
    private const val MAX_DISMISSED = 256
    private const val MAX_ACKS = 256

    /** `NotificationManagerService.MAX_PACKAGE_NOTIFICATIONS`: the 51st post is refused, silently. */
    const val MAX_ACTIVE = 50

    /** A dismissal this close to `postedAt + timeoutMs` counts as the timeout. */
    private const val TIMEOUT_SLACK_MS = 1_000L

    /** Lane / coalescer key of a group summary: ids cannot contain NUL. */
    private const val SUMMARY_KEY = "\u0000group:"

    /** `res/raw/istmo_notification_channels.bin` written by istmo-build. */
    private const val STATIC_CHANNELS_RESOURCE = "istmo_notification_channels"
    private const val CHANNELS_FORMAT = 2

    private class Live(
        var notification: Notification,
        var version: Long,
        var dismissed: Boolean = false,
        /** `elapsedRealtime` of the last post (the timeout counts from it). */
        var postedAtMs: Long = 0,
        /** Whether promotion was asked by the last checked post; `null` = never checked. */
        var promotable: Boolean? = null,
    )

    private class Ack(val id: String, val result: BroadcastReceiver.PendingResult, val reply: Boolean, val timeout: Runnable)

    private class Lane {
        var users = 0
    }

    /** A posted group summary; [signature] `null` when found at start (content unknown). */
    private class Summary(var signature: List<String>?)

    @Volatile
    private var appContext: Context? = null
    private lateinit var renderer: NotificationRenderer
    private lateinit var handler: Handler
    private lateinit var schedule: ScheduleStore
    private val codecs = Responses.codecs

    /** Set once a backend is registered: the runtime runs in this process ([Responses.attach]). */
    @Volatile
    var hostAttached = false

    private val lock = Any()
    private val shown = LinkedHashMap<String, Live>()
    private val dismissedOrder = ArrayDeque<String>()
    private lateinit var coalescer: Coalescer<Notification>
    private var pollArmed = false
    private var nextVersion = 1L

    /** What the system shows: notification id -> its group id. */
    private val active = HashMap<String, String?>()
    private val summaries = HashMap<String, Summary>()
    private val lanes = HashMap<String, Lane>()

    /** Scheduled deliveries taken from the engine, not shown yet: id -> (ticket, group). */
    private val delivering = HashMap<String, Pair<Long, String?>>()
    private var nextTicket = 1L

    /** Channels the plugin knows (default + static + dynamic): defaults below API 26, the unknown-channel check. */
    private val channels = LinkedHashMap<String, ChannelSpec>()
    private val staticChannels = HashSet<String>()
    private val dynamicChannels = LinkedHashMap<String, ChannelSpec>()

    /** System channels the plugin created (deleted when neither static nor dynamic any more). */
    private val ownedChannels = LinkedHashSet<String>()
    private val acks = LinkedHashMap<Long, Ack>()
    private var nextAck = 1L

    private val subscribers = ArrayList<SendChannel<NotificationEvent>>()
    private val backlog = ArrayDeque<NotificationEvent>()

    private val scheduleLock = Any()

    fun error(e: NotificationError): Nothing = throw BackendException(e)

    /** Idempotent; every entry point calls it with any context. */
    fun init(context: Context): Context {
        appContext?.let { return it }
        synchronized(lock) {
            appContext?.let { return it }
            val app = context.applicationContext ?: context
            renderer = NotificationRenderer(app)
            handler = Handler(HandlerThread("istmo-notifications").apply { start() }.looper)
            schedule = ScheduleStore(File(File(app.noBackupFilesDir, "istmo-notifications"), "schedule.bin"), codecs)
            coalescer = Coalescer(Coalescer.ANDROID_PER_SECOND, Coalescer.ANDROID_BURST, SystemClock.elapsedRealtime())
            // Still under the lock: no caller sees a half-loaded state.
            loadChannels(app)
            readActive(app)
            appContext = app
            return app
        }
    }

    private val context: Context get() = appContext ?: throw IllegalStateException("NotificationCenter.init not called")

    private fun manager(): NotificationManager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

    /** Runs [block] on the handler thread; a throw is logged, never kills the thread. */
    private fun later(delayMs: Long, block: () -> Unit): Runnable {
        val r = Runnable {
            try {
                block()
            } catch (e: Exception) {
                Log.w(TAG, "notification task failed", e)
            }
        }
        handler.postDelayed(r, delayMs)
        return r
    }

    /** The active set after a restart: the plugin's notifications and summaries the system still shows. */
    private fun readActive(app: Context) {
        if (Build.VERSION.SDK_INT < 23) return
        val list = try {
            (app.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).activeNotifications
        } catch (e: RuntimeException) {
            Log.w(TAG, "active notifications not readable", e)
            return
        }
        synchronized(lock) {
            for (sbn in list) {
                val tag = sbn.tag ?: continue
                if (sbn.id == SUMMARY_ID && tag.startsWith(SUMMARY_PREFIX)) {
                    summaries.getOrPut(tag.removePrefix(SUMMARY_PREFIX)) { Summary(null) }
                } else if (sbn.id == ITEM_ID && sbn.notification.extras.getBoolean(NotificationRenderer.EXTRA_MARKER)) {
                    if (!active.containsKey(tag)) active[tag] = sbn.notification.group
                }
            }
        }
    }

    // ---- Lanes -----------------------------------------------------------

    /** Runs [block] in [key]'s lane: one at a time per key, in arrival order of the monitor. */
    private inline fun <T> inLane(key: String, block: () -> T): T {
        val lane = synchronized(lock) { lanes.getOrPut(key) { Lane() }.also { it.users++ } }
        try {
            return synchronized(lane) { block() }
        } finally {
            synchronized(lock) {
                lane.users--
                if (lane.users == 0) lanes.remove(key)
            }
        }
    }

    private fun summaryLane(group: String) = SUMMARY_KEY + group

    // ---- Permission ----------------------------------------------------

    fun notificationsAllowed(): Boolean {
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            return false
        }
        return NotificationManagerCompat.from(context).areNotificationsEnabled()
    }

    // ---- Channels ------------------------------------------------------

    private fun channelsFile(context: Context) = File(File(context.noBackupFilesDir, "istmo-notifications"), "channels.bin")

    /**
     * Static channels (the raw resource, owned by `istmo.toml`), then the
     * dynamic ones (`set_channels`, persisted), then `default`. On API 26+
     * every one is created at each start (creating an existing channel
     * only updates its name and description), and system channels the
     * plugin created earlier that are neither static nor dynamic any more
     * (a channel removed from `istmo.toml`) are deleted.
     */
    private fun loadChannels(app: Context) {
        val statics = ArrayList<ChannelSpec>()
        val raw = app.resources.getIdentifier(STATIC_CHANNELS_RESOURCE, "raw", app.packageName)
        if (raw != 0) {
            try {
                val bytes = app.resources.openRawResource(raw).use { it.readBytes() }
                statics.addAll(Bincode.readVec(bytes, 0) { b, o -> codecs.readChannelSpec(b, o) }.value)
            } catch (e: Exception) {
                Log.w(TAG, "static channels not readable", e)
            }
        }
        val (dynamics, owned) = readChannelsFile(app)
        val stale: List<String>
        synchronized(lock) {
            for (spec in statics) {
                channels[spec.id.value] = spec
                staticChannels.add(spec.id.value)
            }
            for (spec in dynamics) {
                if (spec.id.value in staticChannels) continue
                channels[spec.id.value] = spec
                dynamicChannels[spec.id.value] = spec
            }
            if (!channels.containsKey(DEFAULT_CHANNEL)) {
                channels[DEFAULT_CHANNEL] = ChannelSpec(
                    id = ChannelId(DEFAULT_CHANNEL), name = "General", description = null,
                    importance = Interruption.Normal, sound = null, vibration = null, lightColor = null,
                    showBadge = true, lockScreen = null, bypassDnd = false, group = null,
                )
            }
            stale = owned.filter { it !in channels }
            ownedChannels.clear()
            ownedChannels.addAll(channels.keys)
        }
        if (Build.VERSION.SDK_INT >= 26) {
            val manager = app.getSystemService(NotificationManager::class.java)
            for (id in stale) {
                try {
                    manager.deleteNotificationChannel(id)
                } catch (e: RuntimeException) {
                    Log.w(TAG, "stale channel $id not deleted", e)
                }
            }
            for (spec in synchronized(lock) { channels.values.toList() }) createChannel(app, spec)
        }
        if (stale.isNotEmpty() || owned.toSet() != synchronized(lock) { ownedChannels.toSet() }) {
            synchronized(lock) { saveChannels(app) }
        }
    }

    /** Format byte [CHANNELS_FORMAT], wire `Vec<ChannelSpec>` (dynamic), wire `Vec<String>` (owned). */
    private fun readChannelsFile(app: Context): Pair<List<ChannelSpec>, List<String>> {
        val bytes = try {
            channelsFile(app).readBytes()
        } catch (_: java.io.IOException) {
            return emptyList<ChannelSpec>() to emptyList()
        }
        if (bytes.isEmpty() || bytes[0].toInt() != CHANNELS_FORMAT) return emptyList<ChannelSpec>() to emptyList()
        return try {
            val specs = Bincode.readVec(bytes, 1) { b, o -> codecs.readChannelSpec(b, o) }
            val owned = Bincode.readVec(bytes, 1 + specs.consumed) { b, o -> Bincode.readString(b, o) }
            specs.value to owned.value
        } catch (_: IllegalArgumentException) {
            emptyList<ChannelSpec>() to emptyList()
        }
    }

    /** Caller holds [lock]. Only the dynamic channels and the owned ids: statics come from the resource. */
    private fun saveChannels(app: Context) {
        val out = IstmoBytesOut(256)
        out.write(CHANNELS_FORMAT)
        Bincode.writeVec(out, dynamicChannels.values.toList()) { o, c -> codecs.writeChannelSpec(o, c) }
        Bincode.writeVec(out, ownedChannels.toList()) { o, s -> Bincode.writeString(o, s) }
        val file = channelsFile(app)
        try {
            file.parentFile?.mkdirs()
            val tmp = File(file.path + ".tmp")
            tmp.writeBytes(out.toExactArray())
            if (!tmp.renameTo(file)) tmp.delete()
        } catch (e: java.io.IOException) {
            Log.w(TAG, "channels not saved", e)
        }
    }

    private fun createChannel(app: Context, spec: ChannelSpec) {
        if (Build.VERSION.SDK_INT < 26) return
        val manager = app.getSystemService(NotificationManager::class.java)
        spec.group?.let { manager.createNotificationChannelGroup(NotificationChannelGroup(it, it)) }
        val channel = NotificationChannel(spec.id.value, spec.name, NotificationRenderer.importance(spec.importance))
        spec.description?.let { channel.description = it }
        when (val sound = spec.sound) {
            is Sound.Silent -> channel.setSound(null, null)
            is Sound.Named -> renderer.soundUri(sound.value)?.let { channel.setSound(it, NotificationRenderer.soundAttributes) }
            is Sound.Default, null -> {}
        }
        spec.vibration?.let { pattern ->
            channel.enableVibration(true)
            channel.vibrationPattern = pattern.map { it.toLong() }.toLongArray()
        }
        spec.lightColor?.let {
            channel.enableLights(true)
            channel.lightColor = it.toInt()
        }
        channel.setShowBadge(spec.showBadge)
        spec.lockScreen?.let {
            channel.lockscreenVisibility = when (it) {
                LockScreen.Public -> android.app.Notification.VISIBILITY_PUBLIC
                LockScreen.HideContent -> android.app.Notification.VISIBILITY_PRIVATE
                LockScreen.Hidden -> android.app.Notification.VISIBILITY_SECRET
            }
        }
        channel.setBypassDnd(spec.bypassDnd)
        spec.group?.let { channel.group = it }
        manager.createNotificationChannel(channel)
    }

    fun setChannels(specs: List<ChannelSpec>) {
        val app = context
        val created = ArrayList<ChannelSpec>(specs.size)
        synchronized(lock) {
            for (spec in specs) {
                val id = spec.id.value
                // An existing channel keeps its settings except name and description.
                val merged = channels[id]?.copy(name = spec.name, description = spec.description) ?: spec
                channels[id] = merged
                // A static id stays owned by istmo.toml: the next start shows its configured name again.
                if (id !in staticChannels) dynamicChannels[id] = merged
                ownedChannels.add(id)
                created.add(merged)
            }
            saveChannels(app)
        }
        for (spec in created) createChannel(app, spec)
    }

    fun deleteChannel(id: String) {
        val app = context
        synchronized(lock) {
            channels.remove(id)
            dynamicChannels.remove(id)
            ownedChannels.remove(id)
            saveChannels(app)
        }
        if (Build.VERSION.SDK_INT >= 26) app.getSystemService(NotificationManager::class.java).deleteNotificationChannel(id)
    }

    private fun channelOf(n: Notification): Pair<String, ChannelSpec?> {
        val id = n.channel?.value ?: DEFAULT_CHANNEL
        val spec = synchronized(lock) { channels[id] }
        if (Build.VERSION.SDK_INT >= 26) {
            if (spec == null && manager().getNotificationChannel(id) == null) error(NotificationError.UnknownChannel(id))
        } else if (spec == null) {
            error(NotificationError.UnknownChannel(id))
        }
        return id to spec
    }

    // ---- Show / update -------------------------------------------------

    private fun encodeWire(n: Notification): ByteArray? {
        val out = IstmoBytesOut(512)
        codecs.writeNotification(out, n)
        return if (out.size() <= NotificationRenderer.MAX_WIRE_EXTRA) out.toExactArray() else null
    }

    /** Caller holds [lock]. */
    private fun bump(): Long = nextVersion++

    /** Plugin notifications the system shows (estimated below API 23 from the active set). */
    private fun activeCount(): Int {
        if (Build.VERSION.SDK_INT < 23) return synchronized(lock) { active.size + summaries.size }
        return try {
            manager().activeNotifications.size
        } catch (_: RuntimeException) {
            0
        }
    }

    /**
     * Posts version [version] of [n]; [alert] false keeps it silent.
     * Skipped (false) when the table moved on: [n] was cancelled, or
     * newer content was set (which is posted, queued or dropped on its
     * own). Caller holds no lock.
     */
    private fun post(n: Notification, version: Long, alert: Boolean): Boolean {
        val id = n.id.value
        val (channelId, spec) = channelOf(n)
        if (!notificationsAllowed()) error(NotificationError.PermissionDenied)
        val built = renderer.build(n, channelId, spec, alert, encodeWire(n))
        val group = n.group?.id?.value
        val posted = inLane(id) {
            val (current, isNew, needsSummary) = synchronized(lock) {
                val live = shown[id]
                if (live == null || live.version != version) return@inLane false
                val isNew = !active.containsKey(id)
                val members = if (group == null) 0 else active.count { it.value == group && it.key != id }
                Triple(live, isNew, group != null && members >= 1 && !summaries.containsKey(group))
            }
            if (isNew) {
                // The system refuses the 51st notification of an app without telling it.
                val needed = 1 + if (needsSummary) 1 else 0
                if (activeCount() + needed > MAX_ACTIVE) error(NotificationError.TooMany(MAX_ACTIVE.toUInt()))
            }
            try {
                manager().notify(id, ITEM_ID, built)
            } catch (e: SecurityException) {
                error(NotificationError.PermissionDenied)
            } catch (e: RuntimeException) {
                error(NotificationError.Platform(PlatformError(0, e.toString())))
            }
            synchronized(lock) {
                active[id] = group
                current.postedAtMs = SystemClock.elapsedRealtime()
            }
            true
        }
        if (!posted) return false
        n.group?.let { refreshGroup(it.id.value, channelId) }
        if (Api36.promotionSupported) {
            val promotable = Api36.promotable(n)
            val check = synchronized(lock) {
                val live = shown[id]
                val changed = live != null && (alert || live.promotable != promotable)
                if (live != null) live.promotable = promotable
                changed && promotable
            }
            if (check) checkPromotion(id)
        }
        return true
    }

    /**
     * A full show (new or replacing, alerting). [ticket] is the delivery
     * ticket of a scheduled entry: a cancel since it was taken skips it.
     */
    fun show(n: Notification, ticket: Long? = null): Boolean {
        val normalized = normalize(n)
        val id = normalized.id.value
        val (live, previous) = synchronized(lock) {
            if (ticket != null) {
                if (delivering[id]?.first != ticket) return false
                delivering.remove(id)
            }
            coalescer.shown(id, SystemClock.elapsedRealtime())
            dismissedOrder.remove(id)
            val live = Live(normalized, bump())
            live to shown.put(id, live)
        }
        val posted = try {
            post(normalized, live.version, alert = true)
        } catch (e: BackendException) {
            // Nothing new reached the system: the table goes back to what it showed.
            synchronized(lock) {
                if (shown[id] === live) {
                    if (previous != null) shown[id] = previous else shown.remove(id)
                }
            }
            throw e
        }
        if (!posted) return false
        // A replacement in another group leaves the old one a member short.
        val oldGroup = previous?.notification?.group?.id?.value
        if (oldGroup != null && oldGroup != normalized.group?.id?.value) {
            refreshGroup(oldGroup, previous.notification.channel?.value ?: DEFAULT_CHANNEL)
        }
        emit(NotificationEvent.Delivered(normalized.id))
        return true
    }

    private fun normalize(n: Notification): Notification {
        val style = n.style
        if (style is Style.Messaging && style.value.messages.size > NotificationRenderer.MAX_MESSAGES) {
            return n.copy(style = Style.Messaging(style.value.copy(messages = style.value.messages.takeLast(NotificationRenderer.MAX_MESSAGES))))
        }
        return n
    }

    /** The current content of [id]: the table, else the active notification's extras. */
    private fun content(id: String): Notification? {
        synchronized(lock) { shown[id]?.let { return it.notification } }
        if (Build.VERSION.SDK_INT < 23) return null
        val sbn = try {
            manager().activeNotifications.firstOrNull { it.tag == id && it.id == ITEM_ID }
        } catch (_: RuntimeException) {
            null
        } ?: return null
        val wire = sbn.notification.extras.getByteArray(NotificationRenderer.EXTRA_WIRE) ?: return null
        val n = try {
            codecs.readNotification(wire, 0).value
        } catch (_: IllegalArgumentException) {
            return null
        }
        return synchronized(lock) {
            if (!active.containsKey(id)) active[id] = n.group?.id?.value
            shown.getOrPut(id) { Live(n, bump()) }.notification
        }
    }

    /**
     * Changes the shown [id] through the coalescer. A notification only
     * scheduled is changed in the store instead. [NotificationError.NotFound]
     * when it is neither. [transform] may throw (a validation error):
     * nothing changes then.
     */
    fun change(id: String, urgency: Coalescer.Urgency, transform: (Notification) -> Notification) {
        val current = content(id)
        if (current == null) {
            if (changeScheduled(id, transform)) return
            error(NotificationError.NotFound(id))
        }
        val next = normalize(transform(current))
        val (admit, version) = synchronized(lock) {
            val live = shown.getOrPut(id) { Live(next, 0) }
            live.notification = next
            live.version = bump()
            if (urgency != Coalescer.Urgency.REGULAR) {
                live.dismissed = false
                dismissedOrder.remove(id)
            }
            coalescer.offer(id, next, urgency, SystemClock.elapsedRealtime()) to live.version
        }
        when (admit) {
            is Coalescer.Admit.Now -> post(admit.item, version, alert = urgency != Coalescer.Urgency.REGULAR)
            is Coalescer.Admit.Queued -> armPoll()
            is Coalescer.Admit.Dropped -> {}
        }
    }

    /** Posts the table's content of [id] again, silently, through the coalescer (clears a reply spinner). */
    private fun repost(id: String) {
        val (admit, version) = synchronized(lock) {
            val live = shown[id]?.takeIf { !it.dismissed && active.containsKey(id) } ?: return
            coalescer.offer(id, live.notification, Coalescer.Urgency.REGULAR, SystemClock.elapsedRealtime()) to live.version
        }
        when (admit) {
            is Coalescer.Admit.Now -> try {
                post(admit.item, version, alert = false)
            } catch (e: BackendException) {
                Log.w(TAG, "re-post of $id failed: ${e.error}")
            }
            is Coalescer.Admit.Queued -> armPoll()
            is Coalescer.Admit.Dropped -> {}
        }
    }

    private fun armPoll() {
        val due = synchronized(lock) {
            if (pollArmed) return
            val due = coalescer.nextDue() ?: return
            pollArmed = true
            due
        }
        // The coalescer runs on `elapsedRealtime`; handlers on `uptimeMillis`.
        later(maxOf(0L, due - SystemClock.elapsedRealtime()), ::pollQueued)
    }

    private fun pollQueued() {
        val ready = synchronized(lock) {
            pollArmed = false
            coalescer.poll(SystemClock.elapsedRealtime()).mapNotNull { (id, n) ->
                // Queued content is the table's latest (or it was cancelled meanwhile).
                shown[id]?.takeIf { it.notification === n }?.let { n to it.version }
            }
        }
        try {
            for ((n, version) in ready) {
                try {
                    post(n, version, alert = false)
                } catch (e: BackendException) {
                    emit(NotificationEvent.Failed(Failure(n.id, "update not posted: ${e.error}")))
                }
            }
        } finally {
            armPoll()
        }
    }

    private fun checkPromotion(id: String) {
        later(PROMOTION_CHECK_MS) {
            val flag = Api36.flagPromotedOngoing
            val sbn = try {
                manager().activeNotifications.firstOrNull { it.tag == id && it.id == ITEM_ID }
            } catch (_: RuntimeException) {
                null
            } ?: return@later
            if (sbn.notification.flags and flag == 0) emit(NotificationEvent.NotPromoted(NotificationId(id)))
        }
    }

    // ---- Groups --------------------------------------------------------

    private fun summaryTag(group: String) = SUMMARY_PREFIX + group

    /**
     * Posts, keeps or removes the summary of [group]. Posted once the
     * group has 2 members, then kept while it has any: removing a summary
     * makes the system cancel the group's children with it. Re-posted
     * only when the members or their titles change.
     */
    private fun refreshGroup(group: String, channelId: String) {
        inLane(summaryLane(group)) {
            val (known, count, posted) = synchronized(lock) {
                val ids = active.entries.filter { it.value == group }.map { it.key }
                // Members shown before a restart without their content still count.
                Triple(ids.mapNotNull { shown[it]?.notification }, ids.size, summaries[group])
            }
            if (count == 0) {
                if (posted != null) cancelSummary(group)
                return@inLane
            }
            if (posted == null && count < 2) return@inLane
            val latest = known.lastOrNull() ?: return@inLane
            val info = latest.group ?: return@inLane
            val signature = buildList {
                add(info.title.orEmpty())
                add(info.summary.orEmpty())
                for (m in known) {
                    add(m.id.value)
                    add(m.title)
                }
            }
            if (posted?.signature == signature) return@inLane
            synchronized(lock) { coalescer.shown(summaryLane(group), SystemClock.elapsedRealtime()) }
            val summary = renderer.summary(info, channelId, known.asReversed().map { it.title }, latest)
            try {
                manager().notify(summaryTag(group), SUMMARY_ID, summary)
                synchronized(lock) { summaries.getOrPut(group) { Summary(null) }.signature = signature }
            } catch (e: RuntimeException) {
                Log.w(TAG, "group summary not posted", e)
            }
        }
    }

    /** Caller is in [group]'s summary lane. */
    private fun cancelSummary(group: String) {
        synchronized(lock) { summaries.remove(group) }
        try {
            manager().cancel(summaryTag(group), SUMMARY_ID)
        } catch (e: RuntimeException) {
            Log.w(TAG, "group summary not removed", e)
        }
    }

    /** The person dismissed the summary of [group] (its children go with it, each with its delete intent). */
    fun summaryDismissed(group: String) {
        inLane(summaryLane(group)) { synchronized(lock) { summaries.remove(group) } }
    }

    // ---- Cancel / dismiss ----------------------------------------------

    /**
     * Removes [id]: the queued update, the table entry, a delivery in
     * flight (revoked under [scheduleLock], so a due entry taken before
     * the cancel is never shown), then the system notification inside
     * its lane (after a post in flight: cancel wins).
     */
    private fun remove(id: String, unschedule: Boolean, refresh: Boolean = true): Notification? {
        synchronized(scheduleLock) {
            if (unschedule) {
                val engine = ScheduleEngine(schedule.load())
                if (engine.remove(id)) saveAndArm(engine, strict = true)
            }
            synchronized(lock) { delivering.remove(id) }
        }
        val removed = synchronized(lock) {
            coalescer.cancel(id)
            dismissedOrder.remove(id)
            shown.remove(id)?.notification
        }
        val group = inLane(id) {
            val group = synchronized(lock) { active.remove(id) }
            manager().cancel(id, ITEM_ID)
            group
        } ?: removed?.group?.id?.value
        // Its group is a member short (also when its content was never read after a restart).
        if (refresh && group != null) refreshGroup(group, removed?.channel?.value ?: DEFAULT_CHANNEL)
        return removed
    }

    fun cancel(id: String) {
        remove(id, unschedule = true)
    }

    fun cancelGroup(group: String) {
        synchronized(scheduleLock) {
            val engine = ScheduleEngine(schedule.load())
            if (engine.removeGroup(group).isNotEmpty()) saveAndArm(engine, strict = true)
            synchronized(lock) { delivering.entries.removeAll { it.value.second == group } }
        }
        val ids = synchronized(lock) {
            val ids = shown.values.filter { it.notification.group?.id?.value == group }.map { it.notification.id.value }.toMutableSet()
            ids.addAll(active.filterValues { it == group }.keys)
            ids
        }
        if (Build.VERSION.SDK_INT >= 23) {
            for (sbn in manager().activeNotifications) {
                val tag = sbn.tag ?: continue
                if (sbn.id == ITEM_ID && sbn.notification.group == group) ids.add(tag)
            }
        }
        for (id in ids) remove(id, unschedule = false, refresh = false)
        inLane(summaryLane(group)) { cancelSummary(group) }
    }

    fun cancelAll() {
        synchronized(scheduleLock) {
            saveAndArm(ScheduleEngine(), strict = true)
            synchronized(lock) { delivering.clear() }
        }
        val (ids, groups) = synchronized(lock) {
            coalescer.clear()
            dismissedOrder.clear()
            val ids = shown.keys.toMutableSet()
            ids.addAll(active.keys)
            shown.clear()
            ids to summaries.keys.toMutableSet()
        }
        if (Build.VERSION.SDK_INT >= 23) {
            for (sbn in manager().activeNotifications) {
                val tag = sbn.tag ?: continue
                if (sbn.id == SUMMARY_ID && tag.startsWith(SUMMARY_PREFIX)) {
                    groups.add(tag.removePrefix(SUMMARY_PREFIX))
                } else if (sbn.id == ITEM_ID && sbn.notification.extras.getBoolean(NotificationRenderer.EXTRA_MARKER)) {
                    ids.add(tag)
                }
            }
        }
        for (id in ids) remove(id, unschedule = false, refresh = false)
        for (group in groups) inLane(summaryLane(group)) { cancelSummary(group) }
    }

    /** A tap or an action that opened the app removed [id] (the schedule keeps its entry). */
    fun removeShown(id: String) {
        remove(id, unschedule = false)
    }

    /**
     * The delete intent fired: the person dismissed [id], or its
     * `timeoutMs` ran out. Android does not say which; a dismissal within
     * [TIMEOUT_SLACK_MS] of the deadline counts as the timeout (the
     * notification then leaves the table like a removal).
     */
    fun dismissedByUser(id: String) {
        val now = SystemClock.elapsedRealtime()
        var timedOut = false
        val group: String?
        val n = synchronized(lock) {
            group = active.remove(id)
            val live = shown[id]
            if (live == null) {
                // Shown before a restart: later regular updates are still dropped, within the bound.
                if (!coalescer.isDismissed(id)) {
                    coalescer.dismissed(id)
                    dismissedOrder.addLast(id)
                    evictDismissed()
                }
                return@synchronized null
            }
            if (live.dismissed) return
            val timeout = live.notification.timeoutMs
            val shownFor = now - live.postedAtMs
            if (timeout != null && live.postedAtMs > 0 && timeout <= Long.MAX_VALUE.toULong() && shownFor >= timeout.toLong() - TIMEOUT_SLACK_MS) {
                timedOut = true
                shown.remove(id)
                coalescer.cancel(id)
                dismissedOrder.remove(id)
            } else {
                live.dismissed = true
                coalescer.dismissed(id)
                dismissedOrder.addLast(id)
                evictDismissed()
            }
            live.notification
        }
        (n?.group?.id?.value ?: group)?.let { refreshGroup(it, n?.channel?.value ?: DEFAULT_CHANNEL) }
        emit(NotificationEvent.Dismissed(Dismissal(NotificationId(id), if (timedOut) DismissReason.Timeout else DismissReason.User)))
    }

    /** Caller holds [lock]. At most [MAX_DISMISSED] dismissals kept (the core bound). */
    private fun evictDismissed() {
        while (dismissedOrder.size > MAX_DISMISSED) {
            val old = dismissedOrder.removeFirst()
            if (shown[old]?.dismissed == true) shown.remove(old)
            coalescer.cancel(old)
        }
    }

    fun delivered(): List<DeliveredNotification> {
        if (Build.VERSION.SDK_INT < 23) {
            return synchronized(lock) {
                shown.values.filter { !it.dismissed && active.containsKey(it.notification.id.value) }.map { live ->
                    val n = live.notification
                    DeliveredNotification(n.id, n.title, n.body, n.group?.id, null)
                }
            }
        }
        return manager().activeNotifications.mapNotNull { sbn ->
            val tag = sbn.tag ?: return@mapNotNull null
            val extras = sbn.notification.extras
            if (sbn.id != ITEM_ID || !extras.getBoolean(NotificationRenderer.EXTRA_MARKER)) return@mapNotNull null
            DeliveredNotification(
                id = NotificationId(tag),
                title = extras.getCharSequence(android.app.Notification.EXTRA_TITLE)?.toString(),
                body = extras.getCharSequence(android.app.Notification.EXTRA_TEXT)?.toString(),
                group = sbn.notification.group?.let(::GroupId),
                deliveredMs = sbn.postTime,
            )
        }
    }

    // ---- Schedule ------------------------------------------------------

    /** Schedules [n] (validated by the caller); `exact` tells whether the alarm is exact. */
    fun schedule(n: Notification, trigger: Trigger): ScheduledInfo {
        channelOf(n)
        synchronized(scheduleLock) {
            val engine = ScheduleEngine(schedule.load())
            val next = engine.add(normalize(n), trigger, System.currentTimeMillis())
                ?: error(
                    NotificationError.Invalid(
                        InvalidField(
                            if (trigger is Trigger.Calendar && trigger.value.untilMs != null) "trigger.until_ms" else "trigger",
                            Problem.OutOfRange,
                        ),
                    ),
                )
            val exact = saveAndArm(engine, strict = true)
            return ScheduledInfo(exact, next)
        }
    }

    fun pending(): List<PendingNotification> = synchronized(scheduleLock) {
        ScheduleEngine(schedule.load()).entries().map { PendingNotification(it.notification, it.trigger, it.nextFireMs) }
    }

    private fun changeScheduled(id: String, transform: (Notification) -> Notification): Boolean = synchronized(scheduleLock) {
        val engine = ScheduleEngine(schedule.load())
        val entries = engine.entries()
        val entry = entries.firstOrNull { it.notification.id.value == id } ?: return false
        val changed = entries.map { if (it === entry) it.copy(notification = normalize(transform(it.notification))) else it }
        saveAndArm(ScheduleEngine(changed), strict = true)
        true
    }

    /**
     * Caller holds [scheduleLock]. Returns whether the armed alarm is
     * exact. [strict]: a failed save is the caller's error; otherwise
     * (alarms, boot, background threads) it is logged and the alarm is
     * still armed from memory.
     */
    private fun saveAndArm(engine: ScheduleEngine, strict: Boolean): Boolean {
        val entries = engine.entries()
        try {
            schedule.save(entries)
        } catch (e: java.io.IOException) {
            if (strict) error(NotificationError.Platform(PlatformError(0, "schedule not saved: $e")))
            Log.w(TAG, "schedule not saved", e)
        }
        return try {
            Alarms.arm(context, engine.nextDeadline())
        } catch (e: RuntimeException) {
            if (strict) error(NotificationError.Platform(PlatformError(0, "alarm not armed: $e")))
            Log.w(TAG, "alarm not armed", e)
            false
        }
    }

    /** The alarm fired (or the process restarted): delivers what is due, re-arms. Never throws. */
    fun fireDue() {
        val due = try {
            synchronized(scheduleLock) {
                val engine = ScheduleEngine(schedule.load())
                val due = engine.due(System.currentTimeMillis())
                saveAndArm(engine, strict = false)
                // Tickets are taken under the schedule lock: a cancel revokes them there.
                synchronized(lock) {
                    due.map { d ->
                        val ticket = nextTicket++
                        delivering[d.notification.id.value] = ticket to d.notification.group?.id?.value
                        d to ticket
                    }
                }
            }
        } catch (e: Exception) {
            Log.w(TAG, "schedule not read", e)
            return
        }
        for ((d, ticket) in due) {
            try {
                if (show(d.notification, ticket) && d.late) emit(NotificationEvent.DeliveredLate(d.notification.id))
            } catch (e: BackendException) {
                emit(NotificationEvent.Failed(Failure(d.notification.id, "scheduled notification not shown: ${e.error}")))
            } catch (e: Exception) {
                Log.w(TAG, "scheduled notification failed", e)
                emit(NotificationEvent.Failed(Failure(d.notification.id, "scheduled notification not shown: $e")))
            } finally {
                synchronized(lock) { if (delivering[d.notification.id.value]?.first == ticket) delivering.remove(d.notification.id.value) }
            }
        }
    }

    /** Boot, clock or zone change, app update, exact-alarm grant: recompute and re-arm. Never throws. */
    fun rearm(clockChanged: Boolean) {
        try {
            synchronized(scheduleLock) {
                val engine = ScheduleEngine(schedule.load())
                if (clockChanged) engine.recompute(System.currentTimeMillis())
                saveAndArm(engine, strict = false)
            }
        } catch (e: Exception) {
            Log.w(TAG, "schedule not re-armed", e)
        }
        fireDue()
    }

    /** A snooze button: removes [id] and shows it again after [delayMs]. */
    fun snooze(id: String, delayMs: Long) {
        val n = content(id)
        remove(id, unschedule = false)
        if (n != null) {
            try {
                schedule(n, Trigger.After(delayMs.coerceAtLeast(0).toULong()))
            } catch (e: BackendException) {
                emit(NotificationEvent.Failed(Failure(NotificationId(id), "snooze not scheduled: ${e.error}")))
                return
            }
        }
        emit(NotificationEvent.Snoozed(NotificationId(id)))
    }

    // ---- Background responses --------------------------------------------

    /** Holds [result] until `complete(ack)` or 9 s; at most [MAX_ACKS] held (the oldest finished). */
    fun holdAck(id: String, result: BroadcastReceiver.PendingResult, reply: Boolean): AckToken {
        val token = synchronized(lock) { nextAck++ }
        val timeout = Runnable {
            try {
                finishAck(token)
            } catch (e: Exception) {
                Log.w(TAG, "ack timeout failed", e)
            }
        }
        val evicted = synchronized(lock) {
            acks[token] = Ack(id, result, reply, timeout)
            if (acks.size > MAX_ACKS) acks.keys.first() else null
        }
        evicted?.let { finishAck(it) }
        handler.postDelayed(timeout, ACK_TIMEOUT_MS)
        return AckToken(token.toULong())
    }

    /** Finishes the ack [token]; false when it is unknown (completed, timed out or never issued). */
    fun finishAck(token: Long): Boolean {
        val ack = synchronized(lock) { acks.remove(token) } ?: return false
        handler.removeCallbacks(ack.timeout)
        try {
            // A reply leaves a spinner until the notification is posted again.
            if (ack.reply) repost(ack.id)
        } finally {
            ack.result.finish()
        }
        return true
    }

    /** A reply received with no runtime to answer: clears the spinner at once. */
    fun repostAfterReply(id: String) {
        content(id) ?: return
        repost(id)
    }

    // ---- Events --------------------------------------------------------

    fun emit(event: NotificationEvent) {
        synchronized(subscribers) {
            if (subscribers.isEmpty()) {
                if (backlog.size == BACKLOG) backlog.removeFirst()
                backlog.addLast(event)
            } else {
                subscribers.removeAll { it.trySend(event).isClosed }
            }
        }
    }

    fun events(): Flow<NotificationEvent> = callbackFlow {
        synchronized(subscribers) {
            while (backlog.isNotEmpty()) trySend(backlog.removeFirst())
            subscribers.add(channel)
        }
        awaitClose { synchronized(subscribers) { subscribers.remove(channel) } }
    }
}
