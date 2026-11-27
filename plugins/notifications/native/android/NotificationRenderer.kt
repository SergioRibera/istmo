package dev.istmo.plugins.notifications

import android.app.NotificationManager
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.media.AudioAttributes
import android.media.RingtoneManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import androidx.core.app.NotificationCompat
import androidx.core.app.RemoteInput
import androidx.core.graphics.drawable.IconCompat
import android.app.Notification as AndroidNotification
import androidx.core.app.Person as AndroidPerson

/**
 * Builds the platform notification of a [Notification] (validated by the
 * Rust client): `NotificationCompat` styles, actions with their
 * `PendingIntent`s, progress, lock-screen version, and on API 36+ the
 * framework-only parts (`ProgressStyle`, promotion, status chip) through
 * [Api36].
 *
 * Rendering rules (the Android column of the design matrix):
 * - at most [MAX_ACTIONS] buttons; with more, the first two stay and a
 *   "More" action opens the app with the rest (`ResponseAction.More`,
 *   icons stripped to keep the intent small);
 * - replies and choices are `RemoteInput`s (free-form replies from API
 *   24, choices from API 28); below that a reply is a button opening the
 *   app with `inputRequested` and each choice its own button;
 * - `Style.Call` is `NotificationCompat.CallStyle` when the notification
 *   asks for a full-screen intent (API 31+ refuses call-style
 *   notifications that are neither that nor a foreground service), else
 *   a plain notification with the call buttons;
 * - images are decoded here (PNG / JPEG bytes, files, drawables by name)
 *   and scaled down to the size the slot shows.
 */
internal class NotificationRenderer(private val context: Context) {
    private val res = context.resources
    private val pkg = context.packageName

    /** Builds [n] on [channelId]; [alert] false = silent update (`setOnlyAlertOnce`). */
    fun build(n: Notification, channelId: String, channel: ChannelSpec?, alert: Boolean, wire: ByteArray?): AndroidNotification {
        val id = n.id.value
        val b = NotificationCompat.Builder(context, channelId)
            .setSmallIcon(smallIcon(n.android?.smallIcon))
            .setContentTitle(n.title)
            .setOnlyAlertOnce(!alert)
            .setAutoCancel(n.android?.keepOnTap != true)
            .setOngoing(n.ongoing)
            .setDeleteIntent(Intents.dismiss(context, id))
            .setContentIntent(Intents.activity(context, id, "istmo.open", template(n, ResponseAction.Open), cancel = false, mutable = false))
        n.body?.let { b.setContentText(it) }
        n.subtitle?.let { b.setSubText(it) }
        (n.android?.color?.toInt() ?: accent())?.let { b.setColor(it) }
        n.android?.let { a ->
            if (a.colorized) b.setColorized(true)
            a.sortKey?.let { b.setSortKey(it) }
            if (a.localOnly) b.setLocalOnly(true)
        }
        n.icon?.let { image -> bitmap(image, ICON_PX)?.let { b.setLargeIcon(it) } }
        n.category?.let { b.setCategory(category(it)) }
        b.setPriority(priority(n.interruption ?: channel?.importance ?: Interruption.Normal))
        if (Build.VERSION.SDK_INT < 26 && channel != null) legacyChannel(b, n, channel)
        n.timeoutMs?.let { b.setTimeoutAfter(if (it > Long.MAX_VALUE.toULong()) Long.MAX_VALUE else it.toLong()) }
        n.timestampMs?.let { b.setWhen(it).setShowWhen(true) }
        n.chronometer?.let { c ->
            b.setWhen(c.baseMs).setShowWhen(true).setUsesChronometer(true)
            if (Build.VERSION.SDK_INT >= 24) b.setChronometerCountDown(c.countDown)
        }
        n.count?.let { b.setNumber(it.toInt()) }
        n.group?.let { b.setGroup(it.id.value) }
        lockScreen(b, n, channelId)
        if (n.android?.fullScreen == true && canUseFullScreenIntent()) {
            b.setFullScreenIntent(Intents.activity(context, id, "istmo.fullscreen", template(n, ResponseAction.Open), cancel = false, mutable = false), true)
        }

        val progressStyle = Api36.wantsProgressStyle(n)
        val callStyle = style(b, n)
        if (!callStyle) actions(b, n)
        if (n.progress != null && !progressStyle) {
            val p = n.progress
            val value = p.value
            if (value == null) b.setProgress(0, 0, true) else b.setProgress(PROGRESS_MAX, (value.coerceIn(0f, 1f) * PROGRESS_MAX).toInt(), false)
            if (n.body == null) p.label?.let { b.setContentText(it) }
        }

        val extras = Bundle()
        extras.putBoolean(EXTRA_MARKER, true)
        if (wire != null && wire.size <= MAX_WIRE_EXTRA) extras.putByteArray(EXTRA_WIRE, wire)
        b.addExtras(extras)

        val built = b.build()
        return if (Build.VERSION.SDK_INT >= 36) Api36.finish(context, built, n, progressStyle) else built
    }

    /** The group summary posted while [group] has 2 or more members. */
    fun summary(group: Group, channelId: String, titles: List<String>, latest: Notification): AndroidNotification {
        val style = NotificationCompat.InboxStyle()
        for (t in titles.take(6)) style.addLine(t)
        group.summary?.let { style.setSummaryText(it) }
        val title = group.title ?: appLabel()
        return NotificationCompat.Builder(context, channelId)
            .setSmallIcon(smallIcon(latest.android?.smallIcon))
            .setContentTitle(title)
            .setContentText(group.summary ?: titles.firstOrNull())
            .setStyle(style)
            .setGroup(group.id.value)
            .setGroupSummary(true)
            .setGroupAlertBehavior(NotificationCompat.GROUP_ALERT_CHILDREN)
            .setOnlyAlertOnce(true)
            .setAutoCancel(true)
            .setContentIntent(Intents.activity(context, "istmo.group.${group.id.value}", "istmo.open", template(latest, ResponseAction.Open), cancel = false, mutable = false))
            .apply { accent()?.let { setColor(it) } }
            .build()
    }

    // ---- Styles -------------------------------------------------------

    /** Applies [n]'s style; `true` when it is a native call style (which owns the buttons). */
    private fun style(b: NotificationCompat.Builder, n: Notification): Boolean {
        when (val s = n.style) {
            is Style.Plain -> {}
            is Style.BigText -> b.setStyle(NotificationCompat.BigTextStyle().bigText(s.value))
            is Style.Inbox -> {
                val inbox = NotificationCompat.InboxStyle()
                for (line in s.value) inbox.addLine(line)
                b.setStyle(inbox)
            }
            is Style.Picture -> bitmap(s.value, PICTURE_PX)?.let { picture ->
                b.setStyle(NotificationCompat.BigPictureStyle().bigPicture(picture))
            }
            is Style.Messaging -> b.setStyle(messaging(s.value))
            is Style.Call -> return call(b, n, s.value)
        }
        return false
    }

    private fun person(p: Participant): AndroidPerson {
        val builder = AndroidPerson.Builder().setKey(p.key).setName(p.name).setBot(p.isBot).setImportant(p.important)
        p.avatar?.let { image -> bitmap(image, ICON_PX)?.let { builder.setIcon(IconCompat.createWithBitmap(it)) } }
        return builder.build()
    }

    private fun messaging(c: Conversation): NotificationCompat.MessagingStyle {
        val style = NotificationCompat.MessagingStyle(person(c.user))
        c.title?.let { style.setConversationTitle(it) }
        style.setGroupConversation(c.isGroup)
        val people = HashMap<String, AndroidPerson>()
        // Android keeps the last 25 messages; so does the plugin.
        for (m in c.messages.takeLast(MAX_MESSAGES)) {
            val sender = m.sender?.let { s -> people.getOrPut(s.key) { person(s) } }
            style.addMessage(NotificationCompat.MessagingStyle.Message(m.text, m.timestampMs, sender))
        }
        return style
    }

    private fun call(b: NotificationCompat.Builder, n: Notification, info: CallInfo): Boolean {
        val own = n.actions.associateBy { it.id.value }
        fun intent(actionId: String, destructive: Boolean): android.app.PendingIntent {
            val action = own[actionId] ?: callAction(actionId, destructive)
            return actionIntent(n, action, actionId, ResponseAction.Button(action.id), reply = false)
        }
        val native = Build.VERSION.SDK_INT < 31 || (n.android?.fullScreen == true && canUseFullScreenIntent())
        if (!native) {
            // Simulated: the same buttons on a plain notification.
            b.setCategory(NotificationCompat.CATEGORY_CALL)
            val buttons = when (info.kind) {
                CallKind.Ongoing -> listOf(CALL_HANG_UP)
                else -> listOf(CALL_DECLINE, CALL_ANSWER)
            }
            for (key in buttons) {
                val action = own[key] ?: callAction(key, key != CALL_ANSWER)
                b.addAction(NotificationCompat.Action.Builder(null as IconCompat?, action.title, intent(key, key != CALL_ANSWER)).build())
            }
            return true
        }
        val caller = person(info.caller)
        val style = when (info.kind) {
            CallKind.Incoming -> NotificationCompat.CallStyle.forIncomingCall(caller, intent(CALL_DECLINE, true), intent(CALL_ANSWER, false))
            CallKind.Ongoing -> NotificationCompat.CallStyle.forOngoingCall(caller, intent(CALL_HANG_UP, true))
            CallKind.Screening -> NotificationCompat.CallStyle.forScreeningCall(caller, intent(CALL_HANG_UP, true), intent(CALL_ANSWER, false))
        }
        style.setIsVideo(info.video)
        b.setStyle(style)
        return true
    }

    private fun callAction(id: String, destructive: Boolean): Action = Action(
        id = ActionId(id),
        title = when (id) {
            CALL_ANSWER -> "Answer"
            CALL_DECLINE -> "Decline"
            else -> "Hang up"
        },
        kind = ActionKind.Button,
        opensApp = true,
        destructive = destructive,
        requiresUnlock = false,
        url = null,
        icon = null,
    )

    // ---- Actions ------------------------------------------------------

    private fun actions(b: NotificationCompat.Builder, n: Notification) {
        if (n.actions.isEmpty()) return
        val id = n.id.value
        var kept = n.actions
        var overflow: List<Action> = emptyList()
        if (n.actions.size > MAX_ACTIONS) {
            kept = n.actions.take(MAX_ACTIONS - 1)
            overflow = n.actions.drop(MAX_ACTIONS - 1).map { it.copy(icon = null) }
        }
        var shown = 0
        val room = if (overflow.isEmpty()) MAX_ACTIONS else MAX_ACTIONS - 1
        for (action in kept) {
            for (built in action(n, action)) {
                if (shown == room) break
                b.addAction(built)
                shown++
            }
        }
        if (overflow.isNotEmpty()) {
            val response = template(n, ResponseAction.More, overflow = overflow)
            val intent = Intents.activity(context, id, MORE, response, cancel = false, mutable = false)
            b.addAction(NotificationCompat.Action.Builder(null as IconCompat?, MORE_LABEL, intent).build())
        }
    }

    /** The platform actions of [action] (several for simulated choices). */
    private fun action(n: Notification, action: Action): List<NotificationCompat.Action> {
        val id = n.id.value
        val key = action.id.value
        val icon = action.icon?.let(::iconCompat)
        val response = ResponseAction.Button(action.id)
        fun button(title: String, intent: android.app.PendingIntent): NotificationCompat.Action.Builder {
            val builder = NotificationCompat.Action.Builder(icon, title, intent)
            if (action.requiresUnlock) builder.setAuthenticationRequired(true)
            if (action.destructive) builder.setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_DELETE)
            return builder
        }
        return when (val kind = action.kind) {
            is ActionKind.Button -> listOf(button(action.title, actionIntent(n, action, key, response, reply = false)).build())
            is ActionKind.Snooze -> {
                val ms = if (kind.value > Long.MAX_VALUE.toULong()) Long.MAX_VALUE else kind.value.toLong()
                listOf(button(action.title, Intents.snooze(context, id, key, ms)).build())
            }
            is ActionKind.Reply -> {
                if (Build.VERSION.SDK_INT >= 24) {
                    val input = RemoteInput.Builder(Intents.REMOTE_INPUT_KEY)
                        .setLabel(kind.value.placeholder ?: action.title)
                    if (kind.value.suggestions.isNotEmpty()) input.setChoices(kind.value.suggestions.toTypedArray())
                    val builder = button(action.title, actionIntent(n, action, key, response, reply = true))
                        .addRemoteInput(input.build())
                        .setAllowGeneratedReplies(true)
                        .setSemanticAction(NotificationCompat.Action.SEMANTIC_ACTION_REPLY)
                    listOf(builder.build())
                } else {
                    val t = template(n, response, url = action.url, inputRequested = true)
                    listOf(button(action.title, Intents.activity(context, id, key, t, cancel = true, mutable = false)).build())
                }
            }
            is ActionKind.Select -> {
                if (Build.VERSION.SDK_INT >= 28) {
                    val input = RemoteInput.Builder(Intents.REMOTE_INPUT_KEY)
                        .setLabel(action.title)
                        .setChoices(kind.value.toTypedArray())
                        .setAllowFreeFormInput(false)
                    listOf(button(action.title, actionIntent(n, action, key, response, reply = true)).addRemoteInput(input.build()).build())
                } else {
                    kind.value.mapIndexed { i, choice ->
                        val t = template(n, response, url = action.url, reply = choice)
                        val choiceKey = "$key#$i"
                        val intent = if (action.opensApp) {
                            Intents.activity(context, id, choiceKey, t, cancel = n.android?.keepOnTap != true, mutable = false)
                        } else {
                            Intents.background(context, id, choiceKey, t, reply = false, mutable = false)
                        }
                        button(choice, intent).build()
                    }
                }
            }
        }
    }

    private fun actionIntent(n: Notification, action: Action, key: String, response: ResponseAction, reply: Boolean): android.app.PendingIntent {
        val id = n.id.value
        val t = template(n, response, url = action.url)
        return if (action.opensApp) {
            Intents.activity(context, id, key, t, cancel = n.android?.keepOnTap != true, mutable = reply)
        } else {
            Intents.background(context, id, key, t, reply = reply, mutable = reply)
        }
    }

    private fun template(
        n: Notification,
        action: ResponseAction,
        url: String? = null,
        inputRequested: Boolean = false,
        reply: String? = null,
        overflow: List<Action> = emptyList(),
    ): ByteArray = Responses.encode(
        NotificationResponse(
            id = n.id,
            action = action,
            reply = reply,
            inputRequested = inputRequested,
            userData = n.userData,
            url = url ?: n.url,
            launchedApp = false,
            overflowActions = overflow,
            ack = null,
        ),
    )

    // ---- Channel defaults below API 26 ----------------------------------

    @Suppress("DEPRECATION")
    private fun legacyChannel(b: NotificationCompat.Builder, n: Notification, c: ChannelSpec) {
        var defaults = 0
        when (val sound = n.sound ?: c.sound ?: Sound.Default) {
            is Sound.Default -> defaults = defaults or AndroidNotification.DEFAULT_SOUND
            is Sound.Silent -> b.setSilent(true)
            is Sound.Named -> soundUri(sound.value)?.let { b.setSound(it) }
        }
        val vibration = c.vibration
        if (vibration != null) {
            b.setVibrate(vibration.map { it.toLong() }.toLongArray())
        } else {
            defaults = defaults or AndroidNotification.DEFAULT_VIBRATE
        }
        val light = c.lightColor
        if (light != null) b.setLights(light.toInt(), 1_000, 2_000) else defaults = defaults or AndroidNotification.DEFAULT_LIGHTS
        b.setDefaults(defaults)
    }

    private fun lockScreen(b: NotificationCompat.Builder, n: Notification, channelId: String) {
        when (n.lockScreen) {
            LockScreen.Public -> b.setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
            LockScreen.Hidden -> b.setVisibility(NotificationCompat.VISIBILITY_SECRET)
            LockScreen.HideContent -> {
                b.setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
                b.setPublicVersion(
                    NotificationCompat.Builder(context, channelId)
                        .setSmallIcon(smallIcon(n.android?.smallIcon))
                        .setContentTitle(appLabel())
                        .build(),
                )
            }
            null -> {}
        }
    }

    // ---- Resources ----------------------------------------------------

    fun smallIcon(name: String?): Int {
        if (name != null) {
            val named = res.getIdentifier(name, "drawable", pkg)
            if (named != 0) return named
        }
        val generated = res.getIdentifier(SMALL_ICON_RESOURCE, "drawable", pkg)
        if (generated != 0) return generated
        return context.applicationInfo.icon.takeIf { it != 0 } ?: android.R.drawable.ic_dialog_info
    }

    private fun accent(): Int? {
        val color = res.getIdentifier(ACCENT_RESOURCE, "color", pkg)
        return if (color == 0) null else androidx.core.content.ContextCompat.getColor(context, color)
    }

    fun soundUri(name: String): Uri? {
        val raw = res.getIdentifier(name, "raw", pkg)
        return if (raw == 0) null else Uri.parse("android.resource://$pkg/$raw")
    }

    fun appLabel(): String = context.applicationInfo.loadLabel(context.packageManager).toString()

    private fun canUseFullScreenIntent(): Boolean =
        Build.VERSION.SDK_INT < 34 || context.getSystemService(NotificationManager::class.java).canUseFullScreenIntent()

    private fun iconCompat(image: NotificationImage): IconCompat? = when (image) {
        is NotificationImage.Named -> res.getIdentifier(image.value, "drawable", pkg).takeIf { it != 0 }
            ?.let { IconCompat.createWithResource(context, it) }
        else -> bitmap(image, ICON_PX)?.let { IconCompat.createWithBitmap(it) }
    }

    /** Decodes [image] scaled down so its longer side is at most about [maxPx]. */
    fun bitmap(image: NotificationImage, maxPx: Int): Bitmap? {
        val opts = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        fun sample() {
            var size = maxOf(opts.outWidth, opts.outHeight)
            var sample = 1
            while (size > maxPx * 2) {
                size /= 2
                sample *= 2
            }
            opts.inJustDecodeBounds = false
            opts.inSampleSize = sample
        }
        val decoded = when (image) {
            is NotificationImage.Png -> decode(image.value, opts, ::sample)
            is NotificationImage.Jpeg -> decode(image.value, opts, ::sample)
            is NotificationImage.File -> {
                BitmapFactory.decodeFile(image.value, opts)
                if (opts.outWidth <= 0) return null
                sample()
                BitmapFactory.decodeFile(image.value, opts)
            }
            is NotificationImage.Named -> {
                val resId = res.getIdentifier(image.value, "drawable", pkg)
                if (resId == 0) return null
                BitmapFactory.decodeResource(res, resId)
            }
        } ?: return null
        val longest = maxOf(decoded.width, decoded.height)
        if (longest <= maxPx) return decoded
        val scale = maxPx.toFloat() / longest
        return Bitmap.createScaledBitmap(decoded, (decoded.width * scale).toInt().coerceAtLeast(1), (decoded.height * scale).toInt().coerceAtLeast(1), true)
    }

    private fun decode(bytes: ByteArray, opts: BitmapFactory.Options, sample: () -> Unit): Bitmap? {
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, opts)
        if (opts.outWidth <= 0) return null
        sample()
        return BitmapFactory.decodeByteArray(bytes, 0, bytes.size, opts)
    }

    companion object {
        const val MAX_ACTIONS = 3
        const val MAX_MESSAGES = 25
        const val PROGRESS_MAX = 1000
        private const val ICON_PX = 256
        private const val PICTURE_PX = 1440

        /** Marks the plugin's notifications in `getActiveNotifications`. */
        const val EXTRA_MARKER = "dev.istmo.notifications.MARKER"

        /** Wire bytes of the shown `Notification` (when small), to update it after a restart. */
        const val EXTRA_WIRE = "dev.istmo.notifications.WIRE"
        const val MAX_WIRE_EXTRA = 32 * 1024

        /** Resource names `istmo-build` generates (see the design's Android notes). */
        const val SMALL_ICON_RESOURCE = "istmo_notification_icon"
        const val ACCENT_RESOURCE = "istmo_notification_accent"

        const val MORE = "istmo.more"
        private const val MORE_LABEL = "More"
        const val CALL_ANSWER = "istmo.call.answer"
        const val CALL_DECLINE = "istmo.call.decline"
        const val CALL_HANG_UP = "istmo.call.hang_up"

        fun category(c: Category): String = when (c) {
            Category.Message -> NotificationCompat.CATEGORY_MESSAGE
            Category.Call -> NotificationCompat.CATEGORY_CALL
            Category.Alarm -> NotificationCompat.CATEGORY_ALARM
            Category.Reminder -> NotificationCompat.CATEGORY_REMINDER
            Category.Event -> NotificationCompat.CATEGORY_EVENT
            Category.Transfer -> NotificationCompat.CATEGORY_PROGRESS
            Category.Social -> NotificationCompat.CATEGORY_SOCIAL
            Category.Error -> NotificationCompat.CATEGORY_ERROR
            Category.Status -> NotificationCompat.CATEGORY_STATUS
            Category.System -> NotificationCompat.CATEGORY_SYSTEM
            Category.Promo -> NotificationCompat.CATEGORY_PROMO
        }

        fun priority(i: Interruption): Int = when (i) {
            Interruption.Passive -> NotificationCompat.PRIORITY_LOW
            Interruption.Normal -> NotificationCompat.PRIORITY_DEFAULT
            Interruption.High -> NotificationCompat.PRIORITY_HIGH
            Interruption.Urgent, Interruption.Critical -> NotificationCompat.PRIORITY_MAX
        }

        /** Channel importance (API 26+) of [i]. */
        fun importance(i: Interruption): Int = when (i) {
            Interruption.Passive -> NotificationManager.IMPORTANCE_LOW
            Interruption.Normal -> NotificationManager.IMPORTANCE_DEFAULT
            Interruption.High, Interruption.Urgent, Interruption.Critical -> NotificationManager.IMPORTANCE_HIGH
        }

        fun defaultSound(): Uri = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_NOTIFICATION)

        val soundAttributes: AudioAttributes = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_NOTIFICATION)
            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
            .build()
    }
}
