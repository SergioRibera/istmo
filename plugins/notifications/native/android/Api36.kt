package dev.istmo.plugins.notifications

import android.app.NotificationManager
import android.content.Context
import android.os.Build
import android.util.Log
import java.lang.reflect.Method
import android.app.Notification as AndroidNotification

/**
 * Android 16 (API 36) and 16 QPR1 parts, reached by reflection: apps
 * compile against SDK 34 and `androidx.core` 1.13 (newer releases need
 * compileSdk 36), so none of these symbols can be named directly.
 *
 * - `Notification.ProgressStyle` (36): segments, points, indeterminate.
 * - Live Updates (36 QPR1): `Builder.setRequestPromotedOngoing` when the
 *   method exists, else the `android.requestPromotedOngoing` extra it
 *   writes; `Builder.setShortCriticalText` (36) for the status chip.
 * - `NotificationManager.canPostPromotedNotifications()` (36),
 *   `Notification.FLAG_PROMOTED_ONGOING`,
 *   `Settings.ACTION_APP_NOTIFICATION_PROMOTION_SETTINGS`.
 *
 * Every lookup is cached and fails soft: a missing member means the
 * feature is reported unsupported, never a crash.
 */
internal object Api36 {
    private const val TAG = "istmo.notifications"
    private const val EXTRA_REQUEST_PROMOTED_ONGOING = "android.requestPromotedOngoing"

    private val builderClass = AndroidNotification.Builder::class.java

    private val progressStyle: Class<*>? by lazy { find("android.app.Notification\$ProgressStyle") }
    private val segmentClass: Class<*>? by lazy { find("android.app.Notification\$ProgressStyle\$Segment") }
    private val pointClass: Class<*>? by lazy { find("android.app.Notification\$ProgressStyle\$Point") }
    private val setRequestPromoted: Method? by lazy { method(builderClass, "setRequestPromotedOngoing", Boolean::class.javaPrimitiveType!!) }
    private val setShortCriticalText: Method? by lazy { method(builderClass, "setShortCriticalText", String::class.java) }
    private val canPostPromoted: Method? by lazy { method(NotificationManager::class.java, "canPostPromotedNotifications") }

    /** `Notification.FLAG_PROMOTED_ONGOING`, 0 when the platform has none. */
    val flagPromotedOngoing: Int by lazy {
        try {
            AndroidNotification::class.java.getField("FLAG_PROMOTED_ONGOING").getInt(null)
        } catch (_: ReflectiveOperationException) {
            0
        }
    }

    /** `Settings.ACTION_APP_NOTIFICATION_PROMOTION_SETTINGS`, `null` before API 36. */
    val promotionSettingsAction: String? by lazy {
        try {
            android.provider.Settings::class.java.getField("ACTION_APP_NOTIFICATION_PROMOTION_SETTINGS").get(null) as? String
        } catch (_: ReflectiveOperationException) {
            null
        }
    }

    /** Promotion is a platform feature: API 36 with the promoted flag. */
    val promotionSupported: Boolean get() = Build.VERSION.SDK_INT >= 36 && flagPromotedOngoing != 0

    val progressStyleSupported: Boolean get() = Build.VERSION.SDK_INT >= 36 && progressStyle != null

    /** Whether the person lets the app post promoted notifications; `null` when unknown. */
    fun canPostPromoted(context: Context): Boolean? {
        if (!promotionSupported) return null
        val manager = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        return invoke(canPostPromoted, manager) as? Boolean
    }

    /** [n] is drawn with the framework `ProgressStyle` (API 36, plain style, a progress). */
    fun wantsProgressStyle(n: Notification): Boolean =
        n.progress != null && n.style is Style.Plain && progressStyleSupported

    /** Live Updates eligibility the plugin can check (OEMs may add criteria). */
    fun promotable(n: Notification): Boolean =
        n.live && n.ongoing && n.title.isNotEmpty() && n.android?.colorized != true &&
            (n.style is Style.Plain || n.style is Style.BigText || n.style is Style.Call)

    /**
     * Applies the framework-only parts to the built [built] notification;
     * `null` when they could not be applied (reflection fails with
     * checked exceptions too), so the caller can fall back to the compat
     * progress bar.
     */
    fun finish(context: Context, built: AndroidNotification, n: Notification, progress: Boolean): AndroidNotification? {
        val promote = n.live && promotable(n)
        val status = n.statusText
        if (!progress && !promote && status == null) return built
        return try {
            val b = AndroidNotification.Builder.recoverBuilder(context, built)
            if (progress) n.progress?.let { applyProgress(b, it) }
            if (status != null) invoke(setShortCriticalText, b, status)
            if (promote) {
                if (setRequestPromoted != null) {
                    invoke(setRequestPromoted, b, true)
                } else {
                    b.extras.putBoolean(EXTRA_REQUEST_PROMOTED_ONGOING, true)
                }
            }
            b.build()
        } catch (e: Exception) {
            Log.w(TAG, "API 36 notification parts not applied", e)
            null
        }
    }

    private fun applyProgress(b: AndroidNotification.Builder, p: Progress) {
        val styleClass = progressStyle ?: return
        val style = styleClass.getConstructor().newInstance()
        val total = p.segments.sumOf { it.length.toLong() }.toInt()
        val max = if (total > 0) total else NotificationRenderer.PROGRESS_MAX
        if (p.segments.isNotEmpty()) {
            val segment = segmentClass ?: return
            val list = p.segments.map { s ->
                val item = segment.getConstructor(Int::class.javaPrimitiveType).newInstance(s.length.toInt().coerceAtLeast(1))
                s.color?.let { segment.getMethod("setColor", Int::class.javaPrimitiveType).invoke(item, it.toInt()) }
                item
            }
            styleClass.getMethod("setProgressSegments", List::class.java).invoke(style, list)
        } else {
            // One segment of the whole bar, so `max` is the bar's length.
            val segment = segmentClass ?: return
            val item = segment.getConstructor(Int::class.javaPrimitiveType).newInstance(max)
            styleClass.getMethod("setProgressSegments", List::class.java).invoke(style, listOf(item))
        }
        if (p.points.isNotEmpty()) {
            val point = pointClass ?: return
            val list = p.points.map { pt ->
                val item = point.getConstructor(Int::class.javaPrimitiveType).newInstance(pt.position.toInt().coerceIn(0, max))
                pt.color?.let { point.getMethod("setColor", Int::class.javaPrimitiveType).invoke(item, it.toInt()) }
                item
            }
            styleClass.getMethod("setProgressPoints", List::class.java).invoke(style, list)
        }
        val value = p.value
        if (value == null) {
            styleClass.getMethod("setProgressIndeterminate", Boolean::class.javaPrimitiveType).invoke(style, true)
        } else {
            styleClass.getMethod("setProgress", Int::class.javaPrimitiveType).invoke(style, (value.coerceIn(0f, 1f) * max).toInt())
        }
        b.setStyle(style as AndroidNotification.Style)
    }

    private fun find(name: String): Class<*>? = try {
        Class.forName(name)
    } catch (_: ClassNotFoundException) {
        null
    }

    private fun method(owner: Class<*>, name: String, vararg params: Class<*>): Method? = try {
        owner.getMethod(name, *params)
    } catch (_: NoSuchMethodException) {
        null
    }

    private fun invoke(method: Method?, target: Any, vararg args: Any): Any? = try {
        method?.invoke(target, *args)
    } catch (e: Exception) {
        Log.w(TAG, "${method?.name} failed", e)
        null
    }
}
