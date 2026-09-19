package dev.istmo.plugins.datastore

import android.content.Context
import dev.istmo.runtime.DataStoreBackend
import dev.istmo.runtime.DataStoreConfig
import dev.istmo.runtime.DataStoreFactory

class DataStoreFactoryImpl(private val context: Context) : DataStoreFactory {
    override suspend fun create(config: DataStoreConfig): DataStoreBackend =
        DataStoreBackendImpl(context.applicationContext, config)
}

