package dev.istmo.plugins.liveactivity

import android.app.Notification
import android.content.Context
import android.widget.RemoteViews

sealed interface RenderStrategy<in A : Any, in C : Any> {

    class CustomOnly<A : Any, C : Any>(
        val custom: CustomRenderer<A, C>,
    ) : RenderStrategy<A, C>

    class ProgressOnly<A : Any, C : Any>(
        val progress: ProgressRenderer<A, C>,
    ) : RenderStrategy<A, C>

    class Adaptive<A : Any, C : Any>(
        val custom: CustomRenderer<A, C>,
        val progress: ProgressRenderer<A, C>? = null,
        val liveUpdate: LiveUpdateRenderer<A, C>? = null,
    ) : RenderStrategy<A, C>
}

fun interface CustomRenderer<in A : Any, in C : Any> {
    fun render(context: Context, attributes: A, state: C): CustomRender
}

data class CustomRender(
    val collapsed: RemoteViews,
    val expanded: RemoteViews? = null,
    val headsUp: RemoteViews? = null,
)

fun interface ProgressRenderer<in A : Any, in C : Any> {
    @androidx.annotation.RequiresApi(35)
    fun render(context: Context, attributes: A, state: C): Notification.ProgressStyle
}

fun interface LiveUpdateRenderer<in A : Any, in C : Any> {
    @androidx.annotation.RequiresApi(36)
    fun render(context: Context, attributes: A, state: C, builder: Notification.Builder): Notification.Builder
}

