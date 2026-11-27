package dev.istmo.plugins.screencapture

import android.graphics.SurfaceTexture
import android.opengl.EGL14
import android.opengl.EGLConfig
import android.opengl.EGLContext
import android.opengl.EGLDisplay
import android.opengl.EGLExt
import android.opengl.EGLSurface
import android.opengl.GLES11Ext
import android.opengl.GLES20
import android.os.Handler
import android.os.HandlerThread
import android.view.Surface
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.FloatBuffer
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.suspendCancellableCoroutine

/**
 * One capture shown in a preview surface and delivered to a sink
 * (`CopyStage.PreviewFanOut`): the virtual display renders into a
 * `SurfaceTexture` this class owns, and each frame is drawn with GLES
 * into the preview (scaled per [fit]) and into the sink's `ImageReader`
 * surface (stretched to its size, presentation time = the capture time,
 * so the image timestamp stays the capture's `CLOCK_MONOTONIC` instant).
 * One GPU draw per output per frame; no CPU copy.
 *
 * Every GL call runs on this class's own thread.
 */
internal class PreviewFanOut(
    private val preview: Surface,
    private val fit: PreviewFit,
    /** Called once (GL thread) when a GL or `SurfaceTexture` call threw: the session must end. */
    private val onFailed: (String) -> Unit,
) {
    private val thread = HandlerThread("istmo-capture-fanout").apply { start() }
    private val handler = Handler(thread.looper)

    private var display: EGLDisplay = EGL14.EGL_NO_DISPLAY
    private var context: EGLContext = EGL14.EGL_NO_CONTEXT
    private var config: EGLConfig? = null
    private var idle: EGLSurface = EGL14.EGL_NO_SURFACE
    private var previewEgl: EGLSurface = EGL14.EGL_NO_SURFACE
    private var outputEgl: EGLSurface = EGL14.EGL_NO_SURFACE
    private var texture = 0
    private var program = 0
    private var surfaceTexture: SurfaceTexture? = null
    private var input: Surface? = null
    private var contentWidth = 0
    private var contentHeight = 0
    private var released = false
    private var failed = false
    private val matrix = FloatArray(16)

    private val quad: FloatBuffer = floats(-1f, -1f, 1f, -1f, -1f, 1f, 1f, 1f)
    private val texCoords: FloatBuffer = floats(0f, 0f, 1f, 0f, 0f, 1f, 1f, 1f)

    private suspend fun <T> onGl(block: () -> T): T = suspendCancellableCoroutine { cont ->
        handler.post {
            try {
                cont.resume(block())
            } catch (e: Throwable) {
                cont.resumeWithException(e)
            }
        }
    }

    /** Set up EGL, returning the surface the virtual display renders into. */
    suspend fun start(width: Int, height: Int, output: Surface): Surface = onGl {
        display = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY)
        val version = IntArray(2)
        check(EGL14.eglInitialize(display, version, 0, version, 1)) { "eglInitialize failed" }
        val configs = arrayOfNulls<EGLConfig>(1)
        val count = IntArray(1)
        val attribs = intArrayOf(
            EGL14.EGL_RED_SIZE, 8, EGL14.EGL_GREEN_SIZE, 8, EGL14.EGL_BLUE_SIZE, 8, EGL14.EGL_ALPHA_SIZE, 8,
            EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT, EGL_RECORDABLE_ANDROID, 1, EGL14.EGL_NONE,
        )
        check(EGL14.eglChooseConfig(display, attribs, 0, configs, 0, 1, count, 0) && count[0] > 0) {
            "no RGBA8888 GLES2 EGL config"
        }
        config = configs[0]
        context = EGL14.eglCreateContext(
            display, config, EGL14.EGL_NO_CONTEXT, intArrayOf(EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE), 0,
        )
        check(context != EGL14.EGL_NO_CONTEXT) { "eglCreateContext failed" }
        idle = EGL14.eglCreatePbufferSurface(
            display, config, intArrayOf(EGL14.EGL_WIDTH, 1, EGL14.EGL_HEIGHT, 1, EGL14.EGL_NONE), 0,
        )
        check(EGL14.eglMakeCurrent(display, idle, idle, context)) { "eglMakeCurrent failed" }
        program = buildProgram()
        val ids = IntArray(1)
        GLES20.glGenTextures(1, ids, 0)
        texture = ids[0]
        GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, texture)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MIN_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MAG_FILTER, GLES20.GL_LINEAR)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_WRAP_S, GLES20.GL_CLAMP_TO_EDGE)
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_WRAP_T, GLES20.GL_CLAMP_TO_EDGE)
        val texture = SurfaceTexture(texture)
        texture.setOnFrameAvailableListener({ guarded { draw() } }, handler)
        surfaceTexture = texture
        previewEgl = window(preview)
        resizeLocked(width, height, output)
        Surface(texture).also { input = it }
    }

    /**
     * New content size and sink surface (a new reader after a resize),
     * queued on the GL thread ahead of the frame callbacks that follow;
     * [onSwitched] runs (GL thread) once frames go to [output].
     */
    fun resizeLater(width: Int, height: Int, output: Surface, onSwitched: () -> Unit) {
        handler.post {
            if (!released) {
                guarded {
                    resizeLocked(width, height, output)
                    onSwitched()
                }
            }
        }
    }

    /** Run [block]; a call that threw (abandoned producer, lost EGL) ends the session once. */
    private inline fun guarded(block: () -> Unit) {
        try {
            block()
        } catch (e: Throwable) {
            if (!failed) {
                failed = true
                onFailed(e.toString())
            }
        }
    }

    private fun resizeLocked(width: Int, height: Int, output: Surface) {
        contentWidth = width
        contentHeight = height
        surfaceTexture?.setDefaultBufferSize(width, height)
        if (outputEgl != EGL14.EGL_NO_SURFACE) {
            EGL14.eglMakeCurrent(display, idle, idle, context)
            EGL14.eglDestroySurface(display, outputEgl)
        }
        outputEgl = window(output)
    }

    private fun window(surface: Surface): EGLSurface {
        val egl = EGL14.eglCreateWindowSurface(display, config, surface, intArrayOf(EGL14.EGL_NONE), 0)
        check(egl != EGL14.EGL_NO_SURFACE) { "eglCreateWindowSurface failed: 0x${Integer.toHexString(EGL14.eglGetError())}" }
        return egl
    }

    private fun draw() {
        val texture = surfaceTexture ?: return
        if (released || failed) return
        EGL14.eglMakeCurrent(display, idle, idle, context)
        texture.updateTexImage()
        texture.getTransformMatrix(matrix)
        val timestamp = texture.timestamp
        render(previewEgl, fit, null)
        render(outputEgl, PreviewFit.Stretch, timestamp)
    }

    private fun render(target: EGLSurface, fit: PreviewFit, presentationNs: Long?) {
        if (target == EGL14.EGL_NO_SURFACE) return
        if (!EGL14.eglMakeCurrent(display, target, target, context)) return
        val size = IntArray(2)
        EGL14.eglQuerySurface(display, target, EGL14.EGL_WIDTH, size, 0)
        EGL14.eglQuerySurface(display, target, EGL14.EGL_HEIGHT, size, 1)
        GLES20.glClearColor(0f, 0f, 0f, 1f)
        GLES20.glClear(GLES20.GL_COLOR_BUFFER_BIT)
        viewport(fit, size[0], size[1])
        GLES20.glUseProgram(program)
        GLES20.glActiveTexture(GLES20.GL_TEXTURE0)
        GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, texture)
        GLES20.glUniformMatrix4fv(GLES20.glGetUniformLocation(program, "uTex"), 1, false, matrix, 0)
        val position = GLES20.glGetAttribLocation(program, "aPos")
        val coord = GLES20.glGetAttribLocation(program, "aTex")
        GLES20.glEnableVertexAttribArray(position)
        GLES20.glVertexAttribPointer(position, 2, GLES20.GL_FLOAT, false, 0, quad)
        GLES20.glEnableVertexAttribArray(coord)
        GLES20.glVertexAttribPointer(coord, 2, GLES20.GL_FLOAT, false, 0, texCoords)
        GLES20.glDrawArrays(GLES20.GL_TRIANGLE_STRIP, 0, 4)
        if (presentationNs != null) EGLExt.eglPresentationTimeANDROID(display, target, presentationNs)
        EGL14.eglSwapBuffers(display, target)
    }

    private fun viewport(fit: PreviewFit, width: Int, height: Int) {
        if (fit == PreviewFit.Stretch || contentWidth == 0 || contentHeight == 0) {
            GLES20.glViewport(0, 0, width, height)
            return
        }
        val sx = width.toFloat() / contentWidth
        val sy = height.toFloat() / contentHeight
        val scale = if (fit == PreviewFit.Fit) minOf(sx, sy) else maxOf(sx, sy)
        val w = (contentWidth * scale).toInt()
        val h = (contentHeight * scale).toInt()
        GLES20.glViewport((width - w) / 2, (height - h) / 2, w, h)
    }

    private fun buildProgram(): Int {
        val vertex = shader(GLES20.GL_VERTEX_SHADER, VERTEX)
        val fragment = shader(GLES20.GL_FRAGMENT_SHADER, FRAGMENT)
        val id = GLES20.glCreateProgram()
        GLES20.glAttachShader(id, vertex)
        GLES20.glAttachShader(id, fragment)
        GLES20.glLinkProgram(id)
        val status = IntArray(1)
        GLES20.glGetProgramiv(id, GLES20.GL_LINK_STATUS, status, 0)
        check(status[0] == GLES20.GL_TRUE) { "program link failed: ${GLES20.glGetProgramInfoLog(id)}" }
        return id
    }

    private fun shader(kind: Int, source: String): Int {
        val id = GLES20.glCreateShader(kind)
        GLES20.glShaderSource(id, source)
        GLES20.glCompileShader(id)
        val status = IntArray(1)
        GLES20.glGetShaderiv(id, GLES20.GL_COMPILE_STATUS, status, 0)
        check(status[0] == GLES20.GL_TRUE) { "shader compile failed: ${GLES20.glGetShaderInfoLog(id)}" }
        return id
    }

    /** Tear EGL down and stop the thread. Safe to call more than once. */
    fun release() {
        handler.post {
            if (released) return@post
            released = true
            surfaceTexture?.setOnFrameAvailableListener(null)
            if (display != EGL14.EGL_NO_DISPLAY) {
                EGL14.eglMakeCurrent(display, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_CONTEXT)
                for (surface in listOf(previewEgl, outputEgl, idle)) {
                    if (surface != EGL14.EGL_NO_SURFACE) EGL14.eglDestroySurface(display, surface)
                }
                if (context != EGL14.EGL_NO_CONTEXT) EGL14.eglDestroyContext(display, context)
                // The default display is shared with the rest of the
                // process: never terminated here.
                EGL14.eglReleaseThread()
            }
            input?.release()
            surfaceTexture?.release()
            thread.quitSafely()
        }
    }

    private companion object {
        /** `EGL_RECORDABLE_ANDROID`: the config can render into an encoder / reader surface. */
        const val EGL_RECORDABLE_ANDROID = 0x3142

        const val VERTEX = """
            uniform mat4 uTex;
            attribute vec4 aPos;
            attribute vec4 aTex;
            varying vec2 vTex;
            void main() {
                gl_Position = aPos;
                vTex = (uTex * aTex).xy;
            }
        """

        const val FRAGMENT = """
            #extension GL_OES_EGL_image_external : require
            precision mediump float;
            varying vec2 vTex;
            uniform samplerExternalOES sTex;
            void main() {
                gl_FragColor = texture2D(sTex, vTex);
            }
        """

        fun floats(vararg values: Float): FloatBuffer =
            ByteBuffer.allocateDirect(values.size * 4).order(ByteOrder.nativeOrder()).asFloatBuffer().apply {
                put(values)
                position(0)
            }
    }
}
