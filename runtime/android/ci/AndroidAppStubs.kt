@file:Suppress("UNUSED_PARAMETER")

package android.app

open class Service : android.content.Context() {
    open fun onCreate() {}
    open fun onStartCommand(intent: android.content.Intent?, flags: Int, startId: Int): Int = 0
    open fun onDestroy() {}

    companion object {
        const val START_STICKY: Int = 1
    }
}
