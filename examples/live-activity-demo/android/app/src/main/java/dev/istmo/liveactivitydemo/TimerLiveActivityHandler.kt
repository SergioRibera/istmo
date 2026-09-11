package dev.istmo.liveactivitydemo

import android.app.Notification
import android.content.Context
import android.os.SystemClock
import android.widget.RemoteViews
import androidx.core.content.ContextCompat
import dev.istmo.plugins.liveactivity.ChannelConfig
import dev.istmo.plugins.liveactivity.NotificationLiveActivityHandler
import dev.istmo.plugins.liveactivity.RenderStrategy
import dev.istmo.runtime.TimerAttributes
import dev.istmo.runtime.TimerState

/**
 * Demo-specific handler binding `TimerAttributes` + `TimerState` to
 * a three-tier notification renderer.
 *
 * * **Tier 1** (API 21-34) — custom `RemoteViews` layout at
 *   `res/layout/timer_live_{collapsed,expanded}.xml`, wiring the title +
 *   `Chronometer` + `ProgressBar`.
 * * **Tier 2** (API 35+) — `Notification.ProgressStyle` with a single
 *   accent-colored segment, driven directly off `state.elapsed_seconds`.
 * * **Tier 3** (API 36+) — `setPromotedOngoing(true)` +
 *   `setShortCriticalText(state.label)` so the activity anchors as a
 *   status-bar chip (Google's Live Update surface).
 *
 * Payload decoding runs through the generated
 * `dev.istmo.runtime.DemoCodecsImpl` — it wraps the `Bincode` helpers
 * emitted for `TimerAttributes` / `TimerState`, so the handler stays
 * free of hand-rolled wire parsing.
 */
class TimerLiveActivityHandler(context: Context) :
    NotificationLiveActivityHandler<TimerAttributes, TimerState>(
        activityType = "timer",
        channel = ChannelConfig(
            id = "istmo.live_activity.timer",
            name = context.getString(R.string.channel_timer_name),
            description = context.getString(R.string.channel_timer_description),
        ),
        context = context.applicationContext,
    ) {

    private val codecs = dev.istmo.runtime.DemoCodecsImpl()

    override val smallIconRes: Int = R.drawable.ic_timer

    override fun decodeAttributes(bytes: ByteArray): TimerAttributes {
        val decoded = codecs.readTimerAttributes(bytes, 0)
        return decoded.value
    }

    override fun decodeState(bytes: ByteArray): TimerState {
        val decoded = codecs.readTimerState(bytes, 0)
        return decoded.value
    }

    override fun encodeAttributes(attributes: TimerAttributes): ByteArray {
        val out = java.io.ByteArrayOutputStream()
        codecs.writeTimerAttributes(out, attributes)
        return out.toByteArray()
    }

    override fun encodeState(state: TimerState): ByteArray {
        val out = java.io.ByteArrayOutputStream()
        codecs.writeTimerState(out, state)
        return out.toByteArray()
    }

    override fun renderStrategy(): RenderStrategy<TimerAttributes, TimerState> =
        RenderStrategy.Adaptive(
            custom = ::renderCustom,
            progress = ::renderProgress,
            liveUpdate = ::renderLiveUpdate,
        )

    // ---- Tier 1: RemoteViews (API 21+) --------------------------------

    private fun renderCustom(
        context: Context,
        attributes: TimerAttributes,
        state: TimerState,
    ): dev.istmo.plugins.liveactivity.CustomRender {
        val collapsed = buildCollapsed(context, attributes, state)
        val expanded = buildExpanded(context, attributes, state)
        return dev.istmo.plugins.liveactivity.CustomRender(
            collapsed = collapsed,
            expanded = expanded,
            headsUp = expanded,
        )
    }

    private fun buildCollapsed(
        context: Context,
        attributes: TimerAttributes,
        state: TimerState,
    ): RemoteViews = RemoteViews(context.packageName, R.layout.timer_live_collapsed).apply {
        setTextViewText(R.id.timer_title, attributes.title)
        applyChronometer(this, state)
    }

    private fun buildExpanded(
        context: Context,
        attributes: TimerAttributes,
        state: TimerState,
    ): RemoteViews = RemoteViews(context.packageName, R.layout.timer_live_expanded).apply {
        setTextViewText(R.id.timer_title, attributes.title)
        setTextViewText(R.id.timer_label, state.label)
        applyChronometer(this, state)
        val percent = percentComplete(attributes, state)
        setProgressBar(R.id.timer_progress, 100, percent, false)
    }

    private fun applyChronometer(views: RemoteViews, state: TimerState) {
        // `Chronometer` counts up from `base` (in `SystemClock.elapsedRealtime`
        // units). To surface `state.elapsed_seconds` accurately we
        // rebase every update: base = now - elapsed.
        val base = SystemClock.elapsedRealtime() - state.elapsed_seconds.toLong() * 1000L
        views.setChronometer(R.id.timer_elapsed, base, null, true)
    }

    // ---- Tier 2: Notification.ProgressStyle (API 35+) -----------------

    @androidx.annotation.RequiresApi(35)
    private fun renderProgress(
        context: Context,
        attributes: TimerAttributes,
        state: TimerState,
    ): Notification.ProgressStyle {
        val accent = ContextCompat.getColor(context, R.color.timer_accent)
        return Notification.ProgressStyle()
            .setStyledByProgress(true)
            .setProgress(percentComplete(attributes, state))
            .setProgressTrackerIcon(
                android.graphics.drawable.Icon.createWithResource(context, R.drawable.ic_timer),
            )
            .setProgressSegments(
                listOf(
                    Notification.ProgressStyle.Segment(100).setColor(accent),
                ),
            )
    }

    // ---- Tier 3: Live Updates + status bar chip (API 36+) -------------

    @androidx.annotation.RequiresApi(36)
    private fun renderLiveUpdate(
        context: Context,
        attributes: TimerAttributes,
        state: TimerState,
        builder: Notification.Builder,
    ): Notification.Builder {
        val accent = ContextCompat.getColor(context, R.color.timer_accent)
        return builder
            .setPromotedOngoing(true)
            .setShortCriticalText(state.label)
            .setContentTitle(attributes.title)
            .setColorized(true)
            .setColor(accent)
            .setStyle(renderProgress(context, attributes, state))
    }

    private fun percentComplete(attributes: TimerAttributes, state: TimerState): Int {
        val target = attributes.target_seconds.toInt().coerceAtLeast(1)
        val elapsed = state.elapsed_seconds.toInt()
        return ((elapsed.toLong() * 100L) / target).coerceIn(0L, 100L).toInt()
    }
}
