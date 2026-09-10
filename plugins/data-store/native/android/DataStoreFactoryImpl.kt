package dev.istmo.plugins.datastore

import android.content.Context
import dev.istmo.runtime.DataStoreBackend
import dev.istmo.runtime.DataStoreConfig
import dev.istmo.runtime.DataStoreFactory

/**
 * Factory hook consumed by the generated `DataStoreDispatcher`. Each
 * `CreateInstance` frame arriving from Rust invokes [create] with the
 * `DataStoreConfig` shipped from the Rust `acquire_with` call.
 *
 * Register once at app startup:
 *
 *     val dispatcher = DataStoreDispatcher(
 *         factory = DataStoreFactoryImpl(applicationContext),
 *         codecs = DataStoreCodecsImpl(),
 *     )
 *     IstmoRuntime.registerHandler(DataStoreDispatcher.PLUGIN_ID, dispatcher)
 */
class DataStoreFactoryImpl(private val context: Context) : DataStoreFactory {
    override suspend fun create(config: DataStoreConfig): DataStoreBackend =
        DataStoreBackendImpl(context.applicationContext, config)
}
