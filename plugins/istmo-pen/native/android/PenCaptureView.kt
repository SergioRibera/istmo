package dev.istmo.plugins.pen

import android.content.Context
import android.util.AttributeSet
import android.view.MotionEvent
import android.view.View
import dev.istmo.runtime.PenButtonChange
import dev.istmo.runtime.PenCapabilities
import dev.istmo.runtime.PenEvent
import dev.istmo.runtime.PenHoverEvent
import dev.istmo.runtime.PenMove
import dev.istmo.runtime.PenSample
import dev.istmo.runtime.PenToolKind
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import java.util.concurrent.atomic.AtomicInteger

/**
 * Android [View] that intercepts stylus [MotionEvent]s and republishes
 * them as [PenEvent] / [PenHoverEvent] streams for consumption by the
 * `istmo.pen` plugin backend.
 *
 * The view is a passive observer: it does not draw. Add it as an
 * overlay on top of the app's actual drawing surface (or use it as the
 * container itself) so the OS routes stylus input through it.
 * Non-stylus events are handed back to the framework via `super.*`
 * and continue their normal dispatch.
 */
open class PenCaptureView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
    defStyleAttr: Int = 0,
) : View(context, attrs, defStyleAttr) {

    private val eventsFlow = MutableSharedFlow<PenEvent>(
        extraBufferCapacity = 1024,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )
    private val hoverFlow = MutableSharedFlow<PenHoverEvent>(
        extraBufferCapacity = 256,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )
    private val sequence = AtomicInteger(0)
    private val attachTimeMillis: Long = android.os.SystemClock.uptimeMillis()

    init {
        // Hover events only reach us when the view is focusable and
        // the OS treats it as a valid pointer target.
        isFocusable = true
        isFocusableInTouchMode = true
    }

    val events: Flow<PenEvent> = eventsFlow.asSharedFlow()
    val hover: Flow<PenHoverEvent> = hoverFlow.asSharedFlow()

    val capabilities: PenCapabilities = PenCapabilities(
        pressure = true,
        tilt = true,
        azimuth = true,
        altitude = false,
        twist = false,
        tangentialPressure = false,
        hover = true,
        predicted = false,
        coalesced = true,
        barrelButton = true,
        eraser = true,
    )

    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (!isStylusPointer(event)) return super.onTouchEvent(event)
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                emitCoalesced(event) { sample -> PenEvent.Down(sample) }
                emit(PenEvent.Down(sample(event, event.eventTime)))
            }
            MotionEvent.ACTION_MOVE -> {
                val coalesced = collectCoalesced(event)
                emit(
                    PenEvent.Move(
                        PenMove(
                            sample = sample(event, event.eventTime),
                            coalesced = coalesced,
                            predicted = emptyList(),
                        ),
                    ),
                )
            }
            MotionEvent.ACTION_UP -> emit(PenEvent.Up(sample(event, event.eventTime)))
            MotionEvent.ACTION_CANCEL -> emit(PenEvent.Cancel(sample(event, event.eventTime)))
            MotionEvent.ACTION_BUTTON_PRESS,
            MotionEvent.ACTION_BUTTON_RELEASE -> {
                emit(
                    PenEvent.ButtonChanged(
                        PenButtonChange(
                            sample = sample(event, event.eventTime),
                            changed = event.actionButton.toUInt(),
                        ),
                    ),
                )
            }
            else -> {}
        }
        return true
    }

    override fun onGenericMotionEvent(event: MotionEvent): Boolean {
        if (!isStylusPointer(event)) return super.onGenericMotionEvent(event)
        when (event.actionMasked) {
            MotionEvent.ACTION_HOVER_ENTER -> emitHover(
                PenHoverEvent.ProximityEnter(sample(event, event.eventTime)),
            )
            MotionEvent.ACTION_HOVER_MOVE -> emitHover(
                PenHoverEvent.Move(sample(event, event.eventTime)),
            )
            MotionEvent.ACTION_HOVER_EXIT -> emitHover(PenHoverEvent.ProximityLeave)
            else -> return super.onGenericMotionEvent(event)
        }
        return true
    }

    private fun isStylusPointer(event: MotionEvent): Boolean {
        val tool = event.getToolType(0)
        return tool == MotionEvent.TOOL_TYPE_STYLUS ||
            tool == MotionEvent.TOOL_TYPE_ERASER
    }

    private fun collectCoalesced(event: MotionEvent): List<PenSample> {
        val historySize = event.historySize
        if (historySize == 0) return emptyList()
        val out = ArrayList<PenSample>(historySize)
        for (h in 0 until historySize) {
            out.add(historicalSample(event, h))
        }
        return out
    }

    private inline fun emitCoalesced(event: MotionEvent, wrap: (PenSample) -> PenEvent) {
        val historySize = event.historySize
        for (h in 0 until historySize) {
            emit(wrap(historicalSample(event, h)))
        }
    }

    private fun sample(event: MotionEvent, timeMillis: Long): PenSample {
        val tiltAxis = event.getAxisValue(MotionEvent.AXIS_TILT)
        val orientation = event.getAxisValue(MotionEvent.AXIS_ORIENTATION)
        val tiltX = tiltAxis * kotlin.math.sin(orientation)
        val tiltY = -tiltAxis * kotlin.math.cos(orientation)
        val zOffset = event.getAxisValue(MotionEvent.AXIS_DISTANCE)
        return PenSample(
            x = event.x,
            y = event.y,
            pressure = event.pressure,
            tiltX = tiltX,
            tiltY = tiltY,
            azimuth = orientation,
            altitude = 0f,
            twist = 0f,
            tangentialPressure = 0f,
            zOffset = zOffset,
            timestampUs = ((timeMillis - attachTimeMillis).coerceAtLeast(0L) * 1000L).toULong(),
            sequence = sequence.getAndIncrement().toUInt(),
            toolId = event.deviceId.toUInt(),
            toolKind = toolKind(event.getToolType(0)),
            buttons = buttonBitmap(event),
        )
    }

    private fun historicalSample(event: MotionEvent, h: Int): PenSample {
        val tiltAxis = event.getHistoricalAxisValue(MotionEvent.AXIS_TILT, h)
        val orientation = event.getHistoricalAxisValue(MotionEvent.AXIS_ORIENTATION, h)
        val tiltX = tiltAxis * kotlin.math.sin(orientation)
        val tiltY = -tiltAxis * kotlin.math.cos(orientation)
        val zOffset = event.getHistoricalAxisValue(MotionEvent.AXIS_DISTANCE, h)
        val time = event.getHistoricalEventTime(h)
        return PenSample(
            x = event.getHistoricalX(h),
            y = event.getHistoricalY(h),
            pressure = event.getHistoricalPressure(h),
            tiltX = tiltX,
            tiltY = tiltY,
            azimuth = orientation,
            altitude = 0f,
            twist = 0f,
            tangentialPressure = 0f,
            zOffset = zOffset,
            timestampUs = ((time - attachTimeMillis).coerceAtLeast(0L) * 1000L).toULong(),
            sequence = sequence.getAndIncrement().toUInt(),
            toolId = event.deviceId.toUInt(),
            toolKind = toolKind(event.getToolType(0)),
            buttons = buttonBitmap(event),
        )
    }

    private fun toolKind(toolType: Int): PenToolKind = when (toolType) {
        MotionEvent.TOOL_TYPE_ERASER -> PenToolKind.Eraser
        MotionEvent.TOOL_TYPE_STYLUS -> PenToolKind.Tip
        else -> PenToolKind.Unknown
    }

    private fun buttonBitmap(event: MotionEvent): UInt {
        var bits = 0
        if (event.buttonState and MotionEvent.BUTTON_STYLUS_PRIMARY != 0) bits = bits or 1
        if (event.buttonState and MotionEvent.BUTTON_STYLUS_SECONDARY != 0) bits = bits or (1 shl 1)
        return bits.toUInt()
    }

    private fun emit(event: PenEvent) {
        eventsFlow.tryEmit(event)
    }

    private fun emitHover(event: PenHoverEvent) {
        hoverFlow.tryEmit(event)
    }
}
