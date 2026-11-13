// The `IstmoRuntime` surface generated clients, services and workers
// call; the real object needs the Android SDK and the Rust library.
@file:Suppress("UNUSED_PARAMETER")

package dev.istmo.runtime

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.emptyFlow

object IstmoRuntime {
    suspend fun call(pluginId: String, method: String, payload: ByteArray): ByteArray = ByteArray(0)
    suspend fun call(pluginId: String, instanceId: Long, method: String, payload: ByteArray): ByteArray =
        ByteArray(0)
    fun stream(pluginId: String, method: String, payload: ByteArray): Flow<ByteArray> = emptyFlow()
    fun start(): Boolean = true
    fun registerService(pluginId: String, service: android.app.Service) {}
    fun unregisterService(pluginId: String) {}
}
