@file:Suppress("UNUSED_PARAMETER", "unused")

package android.os

class Looper {
    companion object {
        @JvmStatic
        fun getMainLooper(): Looper = Looper()
    }
}

open class Handler(val looper: Looper) {
    fun post(r: Runnable): Boolean = true
    fun postDelayed(r: Runnable, delayMillis: Long): Boolean = true
    fun removeCallbacks(r: Runnable) {}
}

open class HandlerThread(name: String) : Thread(name) {
    val looper: Looper get() = Looper()
    fun quitSafely(): Boolean = true
}

class Process {
    companion object {
        @JvmStatic
        fun myUid(): Int = 0
    }
}
