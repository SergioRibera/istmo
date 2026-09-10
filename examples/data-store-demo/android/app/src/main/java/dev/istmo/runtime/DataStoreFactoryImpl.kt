package dev.istmo.runtime

import android.content.Context

/**
 * Factory hook consumed by the generated `DataStoreDispatcher`. Each
 * `CreateInstance` frame arriving from Rust invokes [create] with the
 * `DataStoreConfig` shipped from the Rust `acquire_with` call.
 */
class DataStoreFactoryImpl(private val context: Context) : DataStoreFactory {
    override suspend fun create(config: DataStoreConfig): DataStoreBackend =
        DataStoreBackendImpl(context.applicationContext, config)
}
