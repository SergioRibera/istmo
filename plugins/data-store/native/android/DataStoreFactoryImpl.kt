package dev.istmo.plugins.datastore

import android.content.Context

class DataStoreFactoryImpl(private val context: Context) : DataStoreFactory {
    override suspend fun create(config: DataStoreConfig): DataStoreBackend =
        DataStoreBackendImpl(context.applicationContext, config)
}

