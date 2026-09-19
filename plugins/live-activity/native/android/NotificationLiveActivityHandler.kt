package dev.istmo.plugins.liveactivity

import android.app.Notification
import android.app.NotificationManager
import android.content.Context
import android.os.Build
import androidx.annotation.RequiresApi
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import dev.istmo.runtime.ActivityError
import dev.istmo.runtime.ActivityStyle
import dev.istmo.runtime.AlertConfig
import dev.istmo.runtime.AlertSound
import dev.istmo.runtime.AndroidTierHint
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.DismissalPolicy
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.NativeHandleId
import dev.istmo.runtime.RestoredActivity
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.ConcurrentHashMap

abstract class NotificationLiveActivityHandler<A : Any, C : Any>(
    override val activityType: String,
    override val channel: ChannelConfig,
    private val context: Context,
) : LiveActivityHandler {

    private val notificationManager: NotificationManager =
        context.getSystemService(NotificationManager::class.java)

    private val notificationIdCounter = AtomicInteger(BASE_NOTIFICATION_ID)

    private val active = ConcurrentHashMap<Long, ActiveEntry<A, C>>()

    private var channelCreated = false

    abstract fun renderStrategy(): RenderStrategy<A, C>

    abstract fun decodeAttributes(bytes: ByteArray): A

    abstract fun decodeState(bytes: ByteArray): C

    open fun encodeAttributes(attributes: A): ByteArray = ByteArray(0)

    open fun encodeState(state: C): ByteArray = ByteArray(0)

    abstract val smallIconRes: Int

    override suspend fun start(
        handle: NativeHandleId,
        attributes: ByteArray,
        initialState: ByteArray,
        style: ActivityStyle,
        staleAfterSeconds: UInt?,
        androidTierHint: AndroidTierHint?,
    ) {
        if (!areNotificationsEnabled()) {
            throw BackendException(ActivityError.Disabled)
        }

        val decodedAttrs = decodeSafe { decodeAttributes(attributes) }
        val decodedState = decodeSafe { decodeState(initialState) }

        val tier = pickTier(androidTierHint) ?: throw BackendException(ActivityError.NotSupported)
        ensureChannel()
        val notificationId = notificationIdCounter.getAndIncrement()

        val notification = buildNotification(
            tier = tier,
            attributes = decodedAttrs,
            state = decodedState,
            alert = null,
        )
        notificationManager.notify(notificationId, notification)

        active[handle] = ActiveEntry(
            notificationId = notificationId,
            attributes = decodedAttrs,
            state = decodedState,
            tier = tier,
        )
    }

    override suspend fun update(
        handle: NativeHandleId,
        state: ByteArray,
        alert: AlertConfig?,
    ) {
        val entry = active[handle] ?: throw BackendException(ActivityError.HandleNotFound)
        val decoded = decodeSafe { decodeState(state) }

        val notification = buildNotification(
            tier = entry.tier,
            attributes = entry.attributes,
            state = decoded,
            alert = alert,
        )
        notificationManager.notify(entry.notificationId, notification)

        active[handle] = entry.copy(state = decoded)
    }

    override suspend fun end(
        handle: NativeHandleId,
        finalState: ByteArray?,
        dismissal: DismissalPolicy,
    ) {
        val entry = active.remove(handle) ?: throw BackendException(ActivityError.HandleNotFound)

        when (dismissal) {
            DismissalPolicy.Immediate -> notificationManager.cancel(entry.notificationId)
            DismissalPolicy.Default -> {
                if (finalState != null) {
                    val decoded = decodeSafe { decodeState(finalState) }
                    notificationManager.notify(
                        entry.notificationId,
                        buildNotification(entry.tier, entry.attributes, decoded, alert = null),
                    )
                }
                notificationManager.cancel(entry.notificationId)
            }
            is DismissalPolicy.AfterSeconds -> {
                if (finalState != null) {
                    val decoded = decodeSafe { decodeState(finalState) }
                    notificationManager.notify(
                        entry.notificationId,
                        buildNotification(entry.tier, entry.attributes, decoded, alert = null),
                    )
                }
                scheduleDismissal(entry.notificationId, dismissal.value.toLong() * 1000L)
            }
        }
    }

    override fun releaseHandle(handle: NativeHandleId) {
        val entry = active.remove(handle) ?: return
        notificationManager.cancel(entry.notificationId)
    }

    private fun pickTier(hint: AndroidTierHint?): Tier? {
        val sdk = Build.VERSION.SDK_INT
        return when (val strat = renderStrategy()) {
            is RenderStrategy.CustomOnly -> Tier.Custom(strat.custom)
            is RenderStrategy.ProgressOnly -> if (sdk >= 35) Tier.Progress(strat.progress) else null
            is RenderStrategy.Adaptive -> when {
                hint == AndroidTierHint.ForceCustom -> Tier.Custom(strat.custom)
                hint == AndroidTierHint.RequireLiveUpdate ->
                    if (sdk >= 36 && strat.liveUpdate != null)
                        Tier.LiveUpdate(strat.liveUpdate, strat.progress, strat.custom)
                    else null
                sdk >= 36 && strat.liveUpdate != null ->
                    Tier.LiveUpdate(strat.liveUpdate, strat.progress, strat.custom)
                sdk >= 35 && strat.progress != null ->
                    Tier.Progress(strat.progress)
                else -> Tier.Custom(strat.custom)
            }
        }
    }

    private fun buildNotification(
        tier: Tier,
        attributes: A,
        state: C,
        alert: AlertConfig?,
    ): Notification = when (tier) {
        is Tier.Custom -> buildCustom(tier.renderer, attributes, state, alert)
        is Tier.Progress -> buildProgress(tier.renderer, attributes, state, alert)
        is Tier.LiveUpdate -> buildLiveUpdate(tier, attributes, state, alert)
    }

    private fun buildCustom(
        renderer: CustomRenderer<A, C>,
        attributes: A,
        state: C,
        alert: AlertConfig?,
    ): Notification {
        val views = renderer.render(context, attributes, state)
        val builder = NotificationCompat.Builder(context, channel.id)
            .setSmallIcon(smallIconRes)
            .setOngoing(true)
            .setOnlyAlertOnce(alert == null)
            .setCustomContentView(views.collapsed)
            .setStyle(NotificationCompat.DecoratedCustomViewStyle())
        views.expanded?.let { builder.setCustomBigContentView(it) }
        views.headsUp?.let { builder.setCustomHeadsUpContentView(it) }
        applyAlert(builder, alert)
        return builder.build()
    }

    @RequiresApi(35)
    private fun buildProgress(
        renderer: ProgressRenderer<A, C>,
        attributes: A,
        state: C,
        alert: AlertConfig?,
    ): Notification {
        val style = renderer.render(context, attributes, state)
        val builder = Notification.Builder(context, channel.id)
            .setSmallIcon(smallIconRes)
            .setOngoing(true)
            .setOnlyAlertOnce(alert == null)
            .setStyle(style)
        applyAlertPlatform(builder, alert)
        return builder.build()
    }

    @RequiresApi(36)
    private fun buildLiveUpdate(
        tier: Tier.LiveUpdate<A, C>,
        attributes: A,
        state: C,
        alert: AlertConfig?,
    ): Notification {
        var builder = Notification.Builder(context, channel.id)
            .setSmallIcon(smallIconRes)
            .setOngoing(true)
            .setOnlyAlertOnce(alert == null)
            .setStyle(tier.progress?.render(context, attributes, state))

        builder = builder.setPromotedOngoing(true)
        applyAlertPlatform(builder, alert)
        return tier.liveUpdate.render(context, attributes, state, builder).build()
    }

    private fun applyAlert(builder: NotificationCompat.Builder, alert: AlertConfig?) {
        if (alert == null) {
            builder.priority = NotificationCompat.PRIORITY_LOW
            return
        }
        builder.priority = NotificationCompat.PRIORITY_HIGH
        builder.setContentTitle(alert.title)
        builder.setContentText(alert.body)
        when (val sound = alert.sound) {
            AlertSound.Default -> builder.setDefaults(NotificationCompat.DEFAULT_SOUND)
            AlertSound.None -> Unit
            is AlertSound.Named -> {
                val uri = android.net.Uri.parse("android.resource://${context.packageName}/raw/${sound.value}")
                builder.setSound(uri)
            }
        }
    }

    @RequiresApi(26)
    private fun applyAlertPlatform(builder: Notification.Builder, alert: AlertConfig?) {
        if (alert == null) return
        builder.setContentTitle(alert.title)
        builder.setContentText(alert.body)
        when (val sound = alert.sound) {
            AlertSound.Default -> builder.setDefaults(Notification.DEFAULT_SOUND)
            AlertSound.None -> Unit
            is AlertSound.Named -> {
                val uri = android.net.Uri.parse("android.resource://${context.packageName}/raw/${sound.value}")
                builder.setSound(uri)
            }
        }
    }

    private fun ensureChannel() {
        if (channelCreated) return
        notificationManager.createNotificationChannel(channel.toChannel())
        channelCreated = true
    }

    private fun areNotificationsEnabled(): Boolean =
        NotificationManagerCompat.from(context).areNotificationsEnabled()

    private inline fun <T> decodeSafe(block: () -> T): T = try {
        block()
    } catch (e: Exception) {
        throw BackendException(ActivityError.Decode(e.message ?: e::class.java.simpleName))
    }

    private fun scheduleDismissal(notificationId: Int, delayMs: Long) {

        Thread {
            try {
                Thread.sleep(delayMs)
                notificationManager.cancel(notificationId)
            } catch (_: InterruptedException) {
                notificationManager.cancel(notificationId)
            }
        }.start()
    }

    private data class ActiveEntry<A : Any, C : Any>(
        val notificationId: Int,
        val attributes: A,
        val state: C,
        val tier: Tier,
    )

    private sealed interface Tier {
        class Custom<A : Any, C : Any>(val renderer: CustomRenderer<A, C>) : Tier

        class Progress<A : Any, C : Any>(val renderer: ProgressRenderer<A, C>) : Tier

        class LiveUpdate<A : Any, C : Any>(
            val liveUpdate: LiveUpdateRenderer<A, C>,
            val progress: ProgressRenderer<A, C>?,
            val custom: CustomRenderer<A, C>,
        ) : Tier
    }

    companion object {
        private const val BASE_NOTIFICATION_ID: Int = 42_000
    }
}

