package dev.istmo.pendemo

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.view.MotionEvent
import dev.istmo.plugins.pen.PenCaptureView

/**
 * Demo-only extension of [PenCaptureView] that renders every stylus
 * stroke to the view's canvas. The plugin base class keeps emitting
 * `PenEvent`s over the istmo wire (so the Rust side still logs them in
 * `adb logcat`); this subclass just adds the visual feedback so the
 * demo is legible on a device without a paired terminal.
 *
 * Kept in the demo (not the plugin) because rendering is not the
 * plugin's job — the plugin is a passive event source.
 */
class PenDrawingView(context: Context) : PenCaptureView(context) {

    private val strokes = mutableListOf<Path>()
    private var currentStroke: Path? = null
    private val ink = Paint().apply {
        color = Color.rgb(0x1a, 0x1a, 0x1a)
        style = Paint.Style.STROKE
        strokeWidth = 4f
        strokeJoin = Paint.Join.ROUND
        strokeCap = Paint.Cap.ROUND
        isAntiAlias = true
    }
    private val eraser = Paint().apply {
        color = Color.WHITE
        style = Paint.Style.STROKE
        strokeWidth = 32f
        strokeJoin = Paint.Join.ROUND
        strokeCap = Paint.Cap.ROUND
        isAntiAlias = true
    }
    private var currentPaint: Paint = ink

    init {
        setBackgroundColor(Color.WHITE)
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        val handled = super.onTouchEvent(event)
        val tool = event.getToolType(0)
        if (tool == MotionEvent.TOOL_TYPE_STYLUS || tool == MotionEvent.TOOL_TYPE_ERASER) {
            when (event.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    currentPaint = if (tool == MotionEvent.TOOL_TYPE_ERASER) eraser else ink
                    val stroke = Path().apply { moveTo(event.x, event.y) }
                    strokes += stroke
                    currentStroke = stroke
                    invalidate()
                }
                MotionEvent.ACTION_MOVE -> {
                    val stroke = currentStroke ?: return handled
                    for (h in 0 until event.historySize) {
                        stroke.lineTo(event.getHistoricalX(h), event.getHistoricalY(h))
                    }
                    stroke.lineTo(event.x, event.y)
                    invalidate()
                }
                MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                    currentStroke = null
                }
                else -> {}
            }
        }
        return handled
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        for (stroke in strokes) {
            canvas.drawPath(stroke, currentPaint)
        }
    }

    fun clearStrokes() {
        strokes.clear()
        currentStroke = null
        invalidate()
    }
}
