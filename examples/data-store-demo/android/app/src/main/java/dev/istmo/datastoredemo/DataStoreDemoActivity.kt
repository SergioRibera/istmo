package dev.istmo.datastoredemo

import dev.istmo.runtime.IstmoNativeActivity

/**
 * Starts the runtime and registers every plugin (the generated
 * `IstmoPluginRegistry` wires istmo-data-store's `DataStoreFactoryImpl`)
 * before `android_main` fires.
 */
class DataStoreDemoActivity : IstmoNativeActivity()
