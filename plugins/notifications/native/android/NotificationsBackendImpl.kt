package dev.istmo.plugins.notifications

import android.Manifest
import android.app.Application
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.core.content.ContextCompat
import dev.istmo.runtime.IstmoHost
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.withContext

/**
 * Android backend of `istmo.notifications` (NotificationCompat, API 21+).
 *
 * Most of the work lives in [NotificationCenter], shared with the
 * receivers that run without a backend; this class answers the calls,
 * owns the `POST_NOTIFICATIONS` prompt ([PermissionPrompt], on the
 * current istmo activity, any activity class) and drains the responses
 * queued while no runtime ran.
 *
 * Context-bound (`android_backend_arg = "context"`): registered once per
 * process with the application context, so it also works under a
 * `NativeActivity`. Every method that touches the system or a file runs
 * on `Dispatchers.IO`.
 */
class NotificationsBackendImpl(context: Context) : NotificationsBackend {
    private val app: Context = NotificationCenter.init(context)
    private val permissionPrompt = PermissionPrompt(app as Application)
    private val prefs by lazy { app.getSharedPreferences("${app.packageName}.istmo.notifications", Context.MODE_PRIVATE) }

    init {
        Responses.attach(app)
        // Alarms may have been lost (force stop clears them) or due.
        Thread({ runCatching { NotificationCenter.rearm(clockChanged = false) } }, "istmo-notifications-start").start()
    }

    // ---- Reports ------------------------------------------------------

    override suspend fun capabilities(): Capabilities {
        val api = Build.VERSION.SDK_INT
        fun n(feature: Feature, native: Boolean = true, otherwise: Support = Support.Unsupported) =
            FeatureSupport(feature, if (native) Support.Native else otherwise)
        val features = listOf(
            n(Feature.Show),
            n(Feature.Update),
            n(Feature.SilentUpdate),
            n(Feature.ProgressBar),
            n(Feature.LivePromotion, Api36.promotionSupported),
            n(Feature.Schedule),
            n(Feature.Repeat),
            FeatureSupport(Feature.Missed, Support.Simulated),
            n(Feature.Actions),
            n(Feature.InlineReply, api >= 24, Support.Simulated),
            n(Feature.Choices, api >= 28, Support.Simulated),
            FeatureSupport(Feature.Snooze, Support.Simulated),
            n(Feature.Avatar),
            n(Feature.Picture),
            n(Feature.Messaging),
            // API 31+: native with `android.fullScreen` (and the grant), simulated otherwise.
            n(Feature.CallStyle),
            n(Feature.Groups),
            n(Feature.Channels, api >= 26, Support.Simulated),
            n(Feature.CustomSound),
            n(Feature.Interruption),
            n(Feature.Ongoing),
            n(Feature.Timeout, api >= 26),
            n(Feature.LockScreen),
            n(Feature.Chronometer),
            n(Feature.ListDelivered, api >= 23, Support.WhileRunning),
            n(Feature.ListPending),
            n(Feature.Dismissal),
            n(Feature.TapWhenClosed),
            n(Feature.BackgroundAction),
            n(Feature.PermissionRequest, api >= 33),
            n(Feature.ExactAlarms, api >= 31),
            n(Feature.DoNotDisturb, api >= 23),
            n(Feature.OpenSettings),
        )
        return Capabilities(
            backend = "android:$api",
            features = features,
            maxActions = NotificationRenderer.MAX_ACTIONS.toUInt(),
            maxPending = null,
            maxActive = MAX_ACTIVE,
        )
    }

    override suspend fun permission(): PermissionStatus = withContext(Dispatchers.IO) { status() }

    private fun declared(permission: String): Boolean {
        @Suppress("DEPRECATION")
        val info = app.packageManager.getPackageInfo(app.packageName, PackageManager.GET_PERMISSIONS)
        return info.requestedPermissions?.contains(permission) == true
    }

    private fun status(): PermissionStatus {
        val allowed = NotificationCenter.notificationsAllowed()
        val api = Build.VERSION.SDK_INT
        val state = when {
            allowed -> PermissionState.Granted
            api >= 33 && ContextCompat.checkSelfPermission(app, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED &&
                !prefs.getBoolean(ASKED, false) -> PermissionState.NotDetermined
            else -> PermissionState.Denied
        }
        val manager = app.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        return PermissionStatus(
            state = state,
            alerts = allowed,
            sound = allowed,
            badge = allowed,
            exactAlarms = Alarms.canExact(app),
            fullScreen = when {
                api >= 34 -> manager.canUseFullScreenIntent()
                else -> declared(USE_FULL_SCREEN_INTENT)
            },
            promoted = Api36.canPostPromoted(app),
            timeSensitive = null,
            communication = null,
            doNotDisturb = if (api >= 23) manager.currentInterruptionFilter > NotificationManager.INTERRUPTION_FILTER_ALL else null,
        )
    }

    override suspend fun request_permission(request: PermissionRequest): PermissionStatus {
        val current = withContext(Dispatchers.IO) { status() }
        if (request.alerts && current.state == PermissionState.NotDetermined && Build.VERSION.SDK_INT >= 33) {
            permissionPrompt.request(Manifest.permission.POST_NOTIFICATIONS)
            prefs.edit().putBoolean(ASKED, true).apply()
        }
        // The other grants are settings pages: opened, not awaited.
        if (request.exactAlarms && current.exactAlarms == false && Build.VERSION.SDK_INT >= 31 && declared(SCHEDULE_EXACT_ALARM)) {
            start(Intent(Settings.ACTION_REQUEST_SCHEDULE_EXACT_ALARM, Uri.parse("package:${app.packageName}")))
        } else if (request.fullScreen && current.fullScreen == false && Build.VERSION.SDK_INT >= 34) {
            start(Intent(Settings.ACTION_MANAGE_APP_USE_FULL_SCREEN_INTENT, Uri.parse("package:${app.packageName}")))
        } else if (request.promoted && current.promoted == false) {
            Api36.promotionSettingsAction?.let { start(Intent(it).putExtra(Settings.EXTRA_APP_PACKAGE, app.packageName)) }
        }
        return withContext(Dispatchers.IO) { status() }
    }

    override suspend fun open_settings(channel: ChannelId?) {
        val pkg = app.packageName
        val intent = when {
            channel != null && Build.VERSION.SDK_INT >= 26 -> Intent(Settings.ACTION_CHANNEL_NOTIFICATION_SETTINGS)
                .putExtra(Settings.EXTRA_APP_PACKAGE, pkg)
                .putExtra(Settings.EXTRA_CHANNEL_ID, channel.value)
            Build.VERSION.SDK_INT >= 26 -> Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, pkg)
            else -> Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:$pkg"))
        }
        start(intent)
    }

    private fun start(intent: Intent) {
        val host = IstmoHost.currentActivity()?.takeUnless { it.isFinishing || it.isDestroyed }
        try {
            if (host != null) {
                host.startActivity(intent)
            } else {
                app.startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            }
        } catch (e: android.content.ActivityNotFoundException) {
            NotificationCenter.error(NotificationError.Platform(PlatformError(0, "no settings page: $e")))
        }
    }

    // ---- Channels -----------------------------------------------------

    override suspend fun set_channels(channels: List<ChannelSpec>) = withContext(Dispatchers.IO) {
        check(Validate.channels(channels))
        NotificationCenter.setChannels(channels)
    }

    override suspend fun delete_channel(id: ChannelId) = withContext(Dispatchers.IO) {
        NotificationCenter.deleteChannel(id.value)
    }

    // ---- Notifications --------------------------------------------------

    override suspend fun show(notification: Notification) = withContext(Dispatchers.IO) {
        check(Validate.notification(notification))
        NotificationCenter.show(notification)
        Unit
    }

    override suspend fun schedule(notification: Notification, trigger: Trigger): ScheduledInfo = withContext(Dispatchers.IO) {
        check(Validate.notification(notification))
        check(Validate.trigger(trigger))
        NotificationCenter.schedule(notification, trigger)
    }

    override suspend fun update(id: NotificationId, patch: NotificationPatch) = withContext(Dispatchers.IO) {
        check(Validate.patch(patch))
        val urgency = if (patch.alertAgain) Coalescer.Urgency.ALERT else Coalescer.Urgency.REGULAR
        // The whole patched notification: call actions need a call style, fields depend on each other.
        NotificationCenter.change(id.value, urgency) { apply(it, patch).also { patched -> check(Validate.notification(patched)) } }
    }

    override suspend fun set_progress(id: NotificationId, progress: Progress?) = withContext(Dispatchers.IO) {
        progress?.let { check(Validate.progress(it)) }
        NotificationCenter.change(id.value, Coalescer.Urgency.REGULAR) { it.copy(progress = progress) }
    }

    override suspend fun append_message(id: NotificationId, message: ChatMessage) = withContext(Dispatchers.IO) {
        check(Validate.message(message))
        NotificationCenter.change(id.value, Coalescer.Urgency.ALERT) { n ->
            val style = n.style as? Style.Messaging
                ?: NotificationCenter.error(NotificationError.Invalid(InvalidField("style", Problem.NotAllowedHere)))
            n.copy(style = Style.Messaging(style.value.copy(messages = style.value.messages + message)))
        }
    }

    override suspend fun cancel(id: NotificationId) = withContext(Dispatchers.IO) { NotificationCenter.cancel(id.value) }

    override suspend fun cancel_group(group: GroupId) = withContext(Dispatchers.IO) { NotificationCenter.cancelGroup(group.value) }

    override suspend fun cancel_all() = withContext(Dispatchers.IO) { NotificationCenter.cancelAll() }

    override suspend fun pending(): List<PendingNotification> = withContext(Dispatchers.IO) { NotificationCenter.pending() }

    override suspend fun delivered(): List<DeliveredNotification> = withContext(Dispatchers.IO) { NotificationCenter.delivered() }

    override suspend fun complete(ack: AckToken) {
        if (!NotificationCenter.finishAck(ack.value.toLong())) NotificationCenter.error(NotificationError.NotFound("ack ${ack.value}"))
    }

    override fun events(): Flow<NotificationEvent> = NotificationCenter.events()

    private companion object {
        const val ASKED = "post_notifications_asked"
        const val USE_FULL_SCREEN_INTENT = "android.permission.USE_FULL_SCREEN_INTENT"
        const val SCHEDULE_EXACT_ALARM = "android.permission.SCHEDULE_EXACT_ALARM"

        /** The system refuses (silently) an app's 51st active notification; `show` answers `TooMany(50)` first. */
        val MAX_ACTIVE: UInt = NotificationCenter.MAX_ACTIVE.toUInt()

        /** An invalid field is the call's `Invalid` error, as on every host. */
        fun check(invalid: InvalidField?) {
            if (invalid != null) NotificationCenter.error(NotificationError.Invalid(invalid))
        }

        /** `Notification::apply`: every set field replaces, `clear` removes. */
        fun apply(n: Notification, p: NotificationPatch): Notification {
            var out = n.copy(
                title = p.title ?: n.title,
                subtitle = p.subtitle ?: n.subtitle,
                body = p.body ?: n.body,
                style = p.style ?: n.style,
                icon = p.icon ?: n.icon,
                actions = p.actions ?: n.actions,
                progress = p.progress ?: n.progress,
                statusText = p.statusText ?: n.statusText,
                count = p.count ?: n.count,
                ongoing = p.ongoing ?: n.ongoing,
                url = p.url ?: n.url,
                userData = p.userData ?: n.userData,
            )
            for (field in p.clear) {
                out = when (field) {
                    PatchField.Subtitle -> out.copy(subtitle = null)
                    PatchField.Body -> out.copy(body = null)
                    PatchField.Icon -> out.copy(icon = null)
                    PatchField.Progress -> out.copy(progress = null)
                    PatchField.StatusText -> out.copy(statusText = null)
                    PatchField.Count -> out.copy(count = null)
                    PatchField.Url -> out.copy(url = null)
                    PatchField.UserData -> out.copy(userData = null)
                    PatchField.Actions -> out.copy(actions = emptyList())
                }
            }
            return out
        }
    }
}
