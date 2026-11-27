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
 * - [Coalescer] at `Rate::ANDROID` (4 per second, burst 4) on a
 *   `HandlerThread`, which also runs the ack timeouts and the promotion
 *   checks.
 * - Schedule: [ScheduleStore] in `noBackupFilesDir`, one `AlarmManager`
 *   alarm for the soonest entry ([Alarms]).
 * - Events: subscribers of `events()`, with a backlog of 64 until the
 *   first one.
 *
 * Notifications are posted with tag = id and id 0; group summaries use
 * the tag `istmo.group:<group id>`.
 */
@SuppressLint("MissingPermission") // POST_NOTIFICATIONS is checked before every post.
internal object NotificationCenter {
    private const val TAG = "istmo.notifications"
    const val DEFAULT_CHANNEL = "default"
    private const val SUMMARY_PREFIX = "istmo.group:"
    private const val BACKLOG = 64
    private const val ACK_TIMEOUT_MS = 9_000L
    private const val PROMOTION_CHECK_MS = 1_000L
    private const val MAX_DISMISSED = 256

    /** `res/raw/istmo_notification_channels.bin` written by istmo-build. */
    private const val STATIC_CHANNELS_RESOURCE = "istmo_notification_channels"
    private const val CHANNELS_FORMAT = 1

    private class Live(var notification: Notification, var dismissed: Boolean = false)

    private class Ack(val id: String, val result: BroadcastReceiver.PendingResult, val reply: Boolean, val timeout: Runnable)

    @Volatile
    private var appContext: Context? = null
    private lateinit var renderer: NotificationRenderer
    private lateinit var handler: Handler
    private lateinit var schedule: ScheduleStore
    private val codecs = Responses.codecs

    /** Set once a backend is registered: the runtime runs in this process. */
    @Volatile
    var hostAttached = false

    private val lock = Any()
    private val shown = LinkedHashMap<String, Live>()
    private val dismissedOrder = ArrayDeque<String>()
    private lateinit var coalescer: Coalescer<Notification>
    private var pollArmed = false

    /** Channels the plugin knows (static + dynamic): defaults below API 26, the unknown-channel check. */
    private val channels = LinkedHashMap<String, ChannelSpec>()
    private val acks = HashMap<Long, Ack>()
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
            loadChannels(app)
            appContext = app
            return app
        }
    }

    private val context: Context get() = appContext ?: throw IllegalStateException("NotificationCenter.init not called")

    private fun manager(): NotificationManager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

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

    private fun dynamicChannelsFile(context: Context) = File(File(context.noBackupFilesDir, "istmo-notifications"), "channels.bin")

    private fun loadChannels(app: Context) {
        val specs = ArrayList<ChannelSpec>()
        val raw = app.resources.getIdentifier(STATIC_CHANNELS_RESOURCE, "raw", app.packageName)
        if (raw != 0) {
            try {
                val bytes = app.resources.openRawResource(raw).use { it.readBytes() }
                specs.addAll(Bincode.readVec(bytes, 0) { b, o -> codecs.readChannelSpec(b, o) }.value)
            } catch (e: RuntimeException) {
                Log.w(TAG, "static channels not readable", e)
            }
        }
        specs.addAll(readDynamicChannels(app))
        for (spec in specs) channels[spec.id.value] = spec
        if (!channels.containsKey(DEFAULT_CHANNEL)) {
            channels[DEFAULT_CHANNEL] = ChannelSpec(
                id = ChannelId(DEFAULT_CHANNEL), name = "General", description = null,
                importance = Interruption.Normal, sound = null, vibration = null, lightColor = null,
                showBadge = true, lockScreen = null, bypassDnd = false, group = null,
            )
        }
        // Static channels are created at every start (creating an existing
        // channel only updates its name and description).
        if (Build.VERSION.SDK_INT >= 26) for (spec in channels.values) createChannel(app, spec)
    }

    private fun readDynamicChannels(app: Context): List<ChannelSpec> {
        val bytes = try {
            dynamicChannelsFile(app).readBytes()
        } catch (_: java.io.IOException) {
            return emptyList()
        }
        if (bytes.isEmpty() || bytes[0].toInt() != CHANNELS_FORMAT) return emptyList()
        return try {
            Bincode.readVec(bytes, 1) { b, o -> codecs.readChannelSpec(b, o) }.value
        } catch (_: IllegalArgumentException) {
            emptyList()
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
        synchronized(lock) {
            for (spec in specs) {
                // An existing channel keeps its settings except name and description.
                val merged = channels[spec.id.value]?.copy(name = spec.name, description = spec.description) ?: spec
                channels[spec.id.value] = merged
            }
            saveDynamicChannels(app)
        }
        for (spec in specs) createChannel(app, spec)
    }

    fun deleteChannel(id: String) {
        val app = context
        synchronized(lock) {
            channels.remove(id)
            saveDynamicChannels(app)
        }
        if (Build.VERSION.SDK_INT >= 26) app.getSystemService(NotificationManager::class.java).deleteNotificationChannel(id)
    }

    /** Caller holds [lock]. Static channels are stored too: harmless, read back at start. */
    private fun saveDynamicChannels(app: Context) {
        val out = IstmoBytesOut(256)
        out.write(CHANNELS_FORMAT)
        Bincode.writeVec(out, channels.values.toList()) { o, c -> codecs.writeChannelSpec(o, c) }
        val file = dynamicChannelsFile(app)
        file.parentFile?.mkdirs()
        val tmp = File(file.path + ".tmp")
        tmp.writeBytes(out.toExactArray())
        if (!tmp.renameTo(file)) tmp.delete()
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

    /** Posts [n] now; [alert] false keeps it silent. Caller does not hold [lock]. */
    private fun post(n: Notification, alert: Boolean) {
        val (channelId, spec) = channelOf(n)
        if (!notificationsAllowed()) error(NotificationError.PermissionDenied)
        val built = renderer.build(n, channelId, spec, alert, encodeWire(n))
        try {
            manager().notify(n.id.value, 0, built)
        } catch (e: SecurityException) {
            error(NotificationError.PermissionDenied)
        } catch (e: RuntimeException) {
            error(NotificationError.Platform(PlatformError(0, e.toString())))
        }
        n.group?.let { refreshGroup(it, channelId) }
        if (n.live && Api36.promotionSupported) checkPromotion(n.id.value)
    }

    /** A full show (new or replacing, alerting). */
    fun show(n: Notification) {
        val normalized = normalize(n)
        val id = normalized.id.value
        synchronized(lock) {
            coalescer.shown(id, SystemClock.elapsedRealtime())
            dismissedOrder.remove(id)
            shown[id] = Live(normalized)
        }
        post(normalized, alert = true)
        emit(NotificationEvent.Delivered(normalized.id))
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
        val active = manager().activeNotifications.firstOrNull { it.tag == id && it.id == 0 } ?: return null
        val wire = active.notification.extras.getByteArray(NotificationRenderer.EXTRA_WIRE) ?: return null
        val n = try {
            codecs.readNotification(wire, 0).value
        } catch (_: IllegalArgumentException) {
            return null
        }
        synchronized(lock) { shown.getOrPut(id) { Live(n) } }
        return n
    }

    /**
     * Changes the shown [id] through the coalescer. A notification only
     * scheduled is changed in the store instead. [NotificationError.NotFound]
     * when it is neither.
     */
    fun change(id: String, urgency: Coalescer.Urgency, transform: (Notification) -> Notification) {
        val current = content(id)
        if (current == null) {
            if (changeScheduled(id, transform)) return
            error(NotificationError.NotFound(id))
        }
        val next = normalize(transform(current))
        val admit = synchronized(lock) {
            val live = shown.getOrPut(id) { Live(next) }
            live.notification = next
            if (urgency != Coalescer.Urgency.REGULAR) {
                live.dismissed = false
                dismissedOrder.remove(id)
            }
            coalescer.offer(id, next, urgency, SystemClock.elapsedRealtime())
        }
        when (admit) {
            is Coalescer.Admit.Now -> post(admit.item, alert = urgency != Coalescer.Urgency.REGULAR)
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
        handler.postDelayed(::pollQueued, maxOf(0L, due - SystemClock.elapsedRealtime()))
    }

    private fun pollQueued() {
        val ready = synchronized(lock) {
            pollArmed = false
            coalescer.poll(SystemClock.elapsedRealtime())
        }
        for ((_, n) in ready) {
            try {
                post(n, alert = false)
            } catch (e: BackendException) {
                emit(NotificationEvent.Failed(Failure(n.id, "update not posted: ${e.error}")))
            }
        }
        armPoll()
    }

    private fun checkPromotion(id: String) {
        handler.postDelayed({
            val flag = Api36.flagPromotedOngoing
            val active = try {
                manager().activeNotifications.firstOrNull { it.tag == id && it.id == 0 }
            } catch (_: RuntimeException) {
                null
            } ?: return@postDelayed
            if (active.notification.flags and flag == 0) emit(NotificationEvent.NotPromoted(NotificationId(id)))
        }, PROMOTION_CHECK_MS)
    }

    // ---- Groups --------------------------------------------------------

    private fun summaryTag(group: String) = SUMMARY_PREFIX + group

    /** Posts or removes the summary of [group]: present while it has 2+ members. */
    private fun refreshGroup(group: Group, channelId: String) {
        val gid = group.id.value
        val members = synchronized(lock) {
            shown.values.filter { !it.dismissed && it.notification.group?.id?.value == gid }.map { it.notification }
        }
        if (members.size >= 2) {
            val summary = renderer.summary(group, channelId, members.asReversed().map { it.title }, members.last())
            try {
                manager().notify(summaryTag(gid), 0, summary)
            } catch (e: RuntimeException) {
                Log.w(TAG, "group summary not posted", e)
            }
        } else {
            manager().cancel(summaryTag(gid), 0)
        }
    }

    private fun refreshGroupOf(n: Notification?) {
        val group = n?.group ?: return
        val channel = n.channel?.value ?: DEFAULT_CHANNEL
        refreshGroup(group, channel)
    }

    // ---- Cancel / dismiss ----------------------------------------------

    fun cancel(id: String) {
        val removed = synchronized(lock) {
            coalescer.cancel(id)
            dismissedOrder.remove(id)
            shown.remove(id)?.notification
        }
        manager().cancel(id, 0)
        refreshGroupOf(removed)
        unschedule(id)
    }

    fun cancelGroup(group: String) {
        val ids = synchronized(lock) {
            shown.values.filter { it.notification.group?.id?.value == group }.map { it.notification.id.value }
        }.toMutableSet()
        if (Build.VERSION.SDK_INT >= 23) {
            for (active in manager().activeNotifications) {
                val tag = active.tag ?: continue
                if (active.notification.group == group && !tag.startsWith(SUMMARY_PREFIX)) ids.add(tag)
            }
        }
        for (id in ids) {
            synchronized(lock) {
                coalescer.cancel(id)
                shown.remove(id)
            }
            manager().cancel(id, 0)
        }
        manager().cancel(summaryTag(group), 0)
        synchronized(scheduleLock) {
            val engine = ScheduleEngine(schedule.load())
            if (engine.removeGroup(group).isNotEmpty()) saveAndArm(engine)
        }
    }

    fun cancelAll() {
        val ids = synchronized(lock) {
            coalescer.clear()
            dismissedOrder.clear()
            val ids = shown.keys.toMutableSet()
            shown.clear()
            ids
        }
        if (Build.VERSION.SDK_INT >= 23) {
            for (active in manager().activeNotifications) {
                if (active.notification.extras.getBoolean(NotificationRenderer.EXTRA_MARKER) ||
                    active.tag?.startsWith(SUMMARY_PREFIX) == true
                ) {
                    active.tag?.let { ids.add(it) }
                }
            }
        }
        for (id in ids) manager().cancel(id, 0)
        synchronized(scheduleLock) { saveAndArm(ScheduleEngine()) }
    }

    /** An action that opened the app removed [id] (the schedule keeps its entry). */
    fun removeShown(id: String) {
        val removed = synchronized(lock) {
            coalescer.cancel(id)
            shown.remove(id)?.notification
        }
        manager().cancel(id, 0)
        refreshGroupOf(removed)
    }

    /** The delete intent fired: the person dismissed [id]. */
    fun dismissedByUser(id: String) {
        val n = synchronized(lock) {
            val live = shown[id] ?: return@synchronized null
            if (live.dismissed) return
            live.dismissed = true
            coalescer.dismissed(id)
            dismissedOrder.addLast(id)
            while (dismissedOrder.size > MAX_DISMISSED) {
                val old = dismissedOrder.removeFirst()
                shown.remove(old)
                coalescer.cancel(old)
            }
            live.notification
        }
        if (n == null) synchronized(lock) { coalescer.dismissed(id) }
        refreshGroupOf(n)
        emit(NotificationEvent.Dismissed(Dismissal(NotificationId(id), DismissReason.User)))
    }

    fun delivered(): List<DeliveredNotification> {
        if (Build.VERSION.SDK_INT < 23) {
            return synchronized(lock) {
                shown.values.filter { !it.dismissed }.map { live ->
                    val n = live.notification
                    DeliveredNotification(n.id, n.title, n.body, n.group?.id, null)
                }
            }
        }
        return manager().activeNotifications.mapNotNull { active ->
            val tag = active.tag ?: return@mapNotNull null
            val extras = active.notification.extras
            if (!extras.getBoolean(NotificationRenderer.EXTRA_MARKER) || tag.startsWith(SUMMARY_PREFIX)) return@mapNotNull null
            DeliveredNotification(
                id = NotificationId(tag),
                title = extras.getCharSequence(android.app.Notification.EXTRA_TITLE)?.toString(),
                body = extras.getCharSequence(android.app.Notification.EXTRA_TEXT)?.toString(),
                group = active.notification.group?.let(::GroupId),
                deliveredMs = active.postTime,
            )
        }
    }

    // ---- Schedule ------------------------------------------------------

    /** Schedules [n]; `exact` tells whether the alarm is exact. */
    fun schedule(n: Notification, trigger: Trigger): ScheduledInfo {
        if (trigger is Trigger.Calendar) {
            val zone = trigger.value.timeZone
            if (zone != null && !CalendarRules.knownZone(zone)) {
                error(NotificationError.Invalid(InvalidField("trigger.time_zone", Problem.OutOfRange)))
            }
        }
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
            val exact = saveAndArm(engine)
            return ScheduledInfo(exact, next)
        }
    }

    fun pending(): List<PendingNotification> = synchronized(scheduleLock) {
        ScheduleEngine(schedule.load()).entries().map { PendingNotification(it.notification, it.trigger, it.nextFireMs) }
    }

    private fun unschedule(id: String) = synchronized(scheduleLock) {
        val engine = ScheduleEngine(schedule.load())
        if (engine.remove(id)) saveAndArm(engine)
    }

    private fun changeScheduled(id: String, transform: (Notification) -> Notification): Boolean = synchronized(scheduleLock) {
        val entries = schedule.load()
        val entry = entries.firstOrNull { it.notification.id.value == id } ?: return false
        val changed = entries.map { if (it === entry) it.copy(notification = normalize(transform(it.notification))) else it }
        saveAndArm(ScheduleEngine(changed))
        true
    }

    /** Caller holds [scheduleLock]. Returns whether the armed alarm is exact. */
    private fun saveAndArm(engine: ScheduleEngine): Boolean {
        val entries = engine.entries()
        try {
            schedule.save(entries)
        } catch (e: java.io.IOException) {
            error(NotificationError.Platform(PlatformError(0, "schedule not saved: $e")))
        }
        return Alarms.arm(context, engine.nextDeadline())
    }

    /** The alarm fired (or the process restarted): delivers what is due, re-arms. */
    fun fireDue() {
        val due = synchronized(scheduleLock) {
            val engine = ScheduleEngine(schedule.load())
            val due = engine.due(System.currentTimeMillis())
            saveAndArm(engine)
            due
        }
        for (d in due) {
            try {
                show(d.notification)
                if (d.late) emit(NotificationEvent.DeliveredLate(d.notification.id))
            } catch (e: BackendException) {
                emit(NotificationEvent.Failed(Failure(d.notification.id, "scheduled notification not shown: ${e.error}")))
            }
        }
    }

    /** Boot, clock or zone change, app update, exact-alarm grant: recompute and re-arm. */
    fun rearm(clockChanged: Boolean) {
        synchronized(scheduleLock) {
            val engine = ScheduleEngine(schedule.load())
            if (clockChanged) engine.recompute(System.currentTimeMillis())
            saveAndArm(engine)
        }
        fireDue()
    }

    /** A snooze button: removes [id] and shows it again after [delayMs]. */
    fun snooze(id: String, delayMs: Long) {
        val n = content(id)
        synchronized(lock) {
            coalescer.cancel(id)
            shown.remove(id)
        }
        manager().cancel(id, 0)
        refreshGroupOf(n)
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

    /** Holds [result] until `complete(ack)` or 9 s. */
    fun holdAck(id: String, result: BroadcastReceiver.PendingResult, reply: Boolean): AckToken {
        val token = synchronized(lock) { nextAck++ }
        val timeout = Runnable { finishAck(token) }
        synchronized(lock) { acks[token] = Ack(id, result, reply, timeout) }
        handler.postDelayed(timeout, ACK_TIMEOUT_MS)
        return AckToken(token.toULong())
    }

    fun finishAck(token: Long) {
        val ack = synchronized(lock) { acks.remove(token) } ?: return
        handler.removeCallbacks(ack.timeout)
        // A reply leaves a spinner until the notification is posted again.
        if (ack.reply) {
            val n = synchronized(lock) { shown[ack.id]?.takeIf { !it.dismissed }?.notification }
            if (n != null) {
                try {
                    post(n, alert = false)
                } catch (_: BackendException) {
                }
            }
        }
        ack.result.finish()
    }

    /** A reply received with no runtime to answer: clears the spinner at once. */
    fun repostAfterReply(id: String) {
        val n = content(id) ?: return
        try {
            post(n, alert = false)
        } catch (_: BackendException) {
        }
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
