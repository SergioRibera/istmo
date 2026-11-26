@file:Suppress("UNUSED_PARAMETER", "unused", "FunctionName")

package android.opengl

class EGLDisplay
class EGLContext
class EGLSurface
class EGLConfig

object EGL14 {
    const val EGL_DEFAULT_DISPLAY: Int = 0
    const val EGL_NONE: Int = 0x3038
    const val EGL_RED_SIZE: Int = 0x3024
    const val EGL_GREEN_SIZE: Int = 0x3023
    const val EGL_BLUE_SIZE: Int = 0x3022
    const val EGL_ALPHA_SIZE: Int = 0x3021
    const val EGL_RENDERABLE_TYPE: Int = 0x3040
    const val EGL_OPENGL_ES2_BIT: Int = 4
    const val EGL_CONTEXT_CLIENT_VERSION: Int = 0x3098
    const val EGL_WIDTH: Int = 0x3057
    const val EGL_HEIGHT: Int = 0x3056

    @JvmField val EGL_NO_DISPLAY: EGLDisplay = EGLDisplay()
    @JvmField val EGL_NO_CONTEXT: EGLContext = EGLContext()
    @JvmField val EGL_NO_SURFACE: EGLSurface = EGLSurface()

    fun eglGetDisplay(displayId: Int): EGLDisplay = EGL_NO_DISPLAY
    fun eglInitialize(dpy: EGLDisplay, major: IntArray, majorOffset: Int, minor: IntArray, minorOffset: Int): Boolean = false
    fun eglChooseConfig(
        dpy: EGLDisplay,
        attribList: IntArray,
        attribListOffset: Int,
        configs: Array<EGLConfig?>,
        configsOffset: Int,
        configSize: Int,
        numConfig: IntArray,
        numConfigOffset: Int,
    ): Boolean = false
    fun eglCreateContext(dpy: EGLDisplay, config: EGLConfig?, shareContext: EGLContext, attribList: IntArray, offset: Int): EGLContext =
        EGL_NO_CONTEXT
    fun eglCreatePbufferSurface(dpy: EGLDisplay, config: EGLConfig?, attribList: IntArray, offset: Int): EGLSurface = EGL_NO_SURFACE
    fun eglCreateWindowSurface(dpy: EGLDisplay, config: EGLConfig?, win: Any, attribList: IntArray, offset: Int): EGLSurface =
        EGL_NO_SURFACE
    fun eglMakeCurrent(dpy: EGLDisplay, draw: EGLSurface, read: EGLSurface, ctx: EGLContext): Boolean = false
    fun eglQuerySurface(dpy: EGLDisplay, surface: EGLSurface, attribute: Int, value: IntArray, offset: Int): Boolean = false
    fun eglSwapBuffers(dpy: EGLDisplay, surface: EGLSurface): Boolean = false
    fun eglDestroySurface(dpy: EGLDisplay, surface: EGLSurface): Boolean = false
    fun eglDestroyContext(dpy: EGLDisplay, ctx: EGLContext): Boolean = false
    fun eglReleaseThread(): Boolean = false
    fun eglGetError(): Int = 0
}

object EGLExt {
    fun eglPresentationTimeANDROID(dpy: EGLDisplay, sur: EGLSurface, time: Long): Boolean = false
}

object GLES11Ext {
    const val GL_TEXTURE_EXTERNAL_OES: Int = 0x8D65
}

object GLES20 {
    const val GL_TRUE: Int = 1
    const val GL_FLOAT: Int = 0x1406
    const val GL_TRIANGLE_STRIP: Int = 5
    const val GL_COLOR_BUFFER_BIT: Int = 0x4000
    const val GL_TEXTURE0: Int = 0x84C0
    const val GL_TEXTURE_MIN_FILTER: Int = 0x2801
    const val GL_TEXTURE_MAG_FILTER: Int = 0x2800
    const val GL_TEXTURE_WRAP_S: Int = 0x2802
    const val GL_TEXTURE_WRAP_T: Int = 0x2803
    const val GL_LINEAR: Int = 0x2601
    const val GL_CLAMP_TO_EDGE: Int = 0x812F
    const val GL_VERTEX_SHADER: Int = 0x8B31
    const val GL_FRAGMENT_SHADER: Int = 0x8B30
    const val GL_COMPILE_STATUS: Int = 0x8B81
    const val GL_LINK_STATUS: Int = 0x8B82

    fun glGenTextures(n: Int, textures: IntArray, offset: Int) {}
    fun glBindTexture(target: Int, texture: Int) {}
    fun glTexParameteri(target: Int, pname: Int, param: Int) {}
    fun glClearColor(red: Float, green: Float, blue: Float, alpha: Float) {}
    fun glClear(mask: Int) {}
    fun glViewport(x: Int, y: Int, width: Int, height: Int) {}
    fun glUseProgram(program: Int) {}
    fun glActiveTexture(texture: Int) {}
    fun glUniformMatrix4fv(location: Int, count: Int, transpose: Boolean, value: FloatArray, offset: Int) {}
    fun glGetUniformLocation(program: Int, name: String): Int = 0
    fun glGetAttribLocation(program: Int, name: String): Int = 0
    fun glEnableVertexAttribArray(index: Int) {}
    fun glVertexAttribPointer(indx: Int, size: Int, type: Int, normalized: Boolean, stride: Int, ptr: java.nio.Buffer) {}
    fun glDrawArrays(mode: Int, first: Int, count: Int) {}
    fun glCreateShader(type: Int): Int = 0
    fun glShaderSource(shader: Int, string: String) {}
    fun glCompileShader(shader: Int) {}
    fun glGetShaderiv(shader: Int, pname: Int, params: IntArray, offset: Int) {}
    fun glGetShaderInfoLog(shader: Int): String = ""
    fun glCreateProgram(): Int = 0
    fun glAttachShader(program: Int, shader: Int) {}
    fun glLinkProgram(program: Int) {}
    fun glGetProgramiv(program: Int, pname: Int, params: IntArray, offset: Int) {}
    fun glGetProgramInfoLog(program: Int): String = ""
}
