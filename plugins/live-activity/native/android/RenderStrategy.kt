package dev.istmo.plugins.liveactivity

import android.app.Notification
import android.content.Context
import android.widget.RemoteViews

/**
 * How a [LiveActivityHandler] wants its notification rendered on the
 * current Android device.
 *
 * Three renderers are available; the backend picks the highest tier the
 * device supports:
 *
 * * **Custom** — `RemoteViews`-backed ongoing notification. API 21+.
 * * **Progress** — `Notification.ProgressStyle` system template. API 35+ (Android 15).
 * * **LiveUpdate** — promoted ongoing + status-bar chip anchor. API 36+ (Android 16).
 *
 * [Adaptive] is the recommended shape — supply the highest-tier builders
 * available and the backend falls back automatically. [CustomOnly] pins
 * the surface to `RemoteViews` for a uniform look across every Android
 * version. [ProgressOnly] short-circuits to the system template.
 */
sealed interface RenderStrategy<in A : Any, in C : Any> {

    /**
     * `RemoteViews` on every Android version regardless of what the OS
     * supports. Useful when the design system demands a uniform surface.
     */
    class CustomOnly<A : Any, C : Any>(
        val custom: CustomRenderer<A, C>,
    ) : RenderStrategy<A, C>

    /**
     * `Notification.ProgressStyle` on API 35+ only. Consumers who take
     * this path must be OK with surfacing [ActivityError.NotSupported]
     * on older devices.
     */
    class ProgressOnly<A : Any, C : Any>(
        val progress: ProgressRenderer<A, C>,
    ) : RenderStrategy<A, C>

    /**
     * Adaptive rendering — the backend picks the highest tier supported
     * by the current device. `liveUpdate` and `progress` are optional;
     * `custom` is mandatory as the universal fallback (API 21+).
     */
    class Adaptive<A : Any, C : Any>(
        val custom: CustomRenderer<A, C>,
        val progress: ProgressRenderer<A, C>? = null,
        val liveUpdate: LiveUpdateRenderer<A, C>? = null,
    ) : RenderStrategy<A, C>
}

/**
 * Tier 1 renderer — builds a `RemoteViews` layout for the ongoing
 * notification's content / bigContent / heads-up surfaces.
 *
 * Returning `null` for `heads-up` reuses the collapsed content view; the
 * platform still promotes the notification when priority is HIGH.
 */
fun interface CustomRenderer<in A : Any, in C : Any> {
    fun render(context: Context, attributes: A, state: C): CustomRender
}

/** Tier 1 render output — three optional `RemoteViews` surfaces. */
data class CustomRender(
    val collapsed: RemoteViews,
    val expanded: RemoteViews? = null,
    val headsUp: RemoteViews? = null,
)

/**
 * Tier 2 renderer — populates a [Notification.ProgressStyle] instance.
 * Only invoked on API 35+ devices when a `progress` renderer is
 * registered.
 */
fun interface ProgressRenderer<in A : Any, in C : Any> {
    @androidx.annotation.RequiresApi(35)
    fun render(context: Context, attributes: A, state: C): Notification.ProgressStyle
}

/**
 * Tier 3 renderer — decorates the `Notification.Builder` for API 36+
 * Live Updates (`setPromotedOngoing(true)` + status-bar chip anchor).
 * The backend passes in a builder already primed with channel + basic
 * fields; the renderer applies style + colors + short critical text.
 */
fun interface LiveUpdateRenderer<in A : Any, in C : Any> {
    @androidx.annotation.RequiresApi(36)
    fun render(context: Context, attributes: A, state: C, builder: Notification.Builder): Notification.Builder
}
