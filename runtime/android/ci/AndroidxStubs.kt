@file:Suppress("UNUSED_PARAMETER")

package androidx.lifecycle

open class LifecycleService : android.app.Service()

val LifecycleService.lifecycleScope: kotlinx.coroutines.CoroutineScope
    get() = kotlinx.coroutines.CoroutineScope(kotlinx.coroutines.Dispatchers.Unconfined)
