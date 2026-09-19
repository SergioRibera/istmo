package dev.istmo.datastoredemo

import android.app.NativeActivity
import android.os.Bundle
import dev.istmo.runtime.DataStoreCodecsImpl
import dev.istmo.runtime.DataStoreDispatcher
import dev.istmo.runtime.DataStoreFactoryImpl
import dev.istmo.runtime.IstmoRuntime

class DataStoreDemoActivity : NativeActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        val runtime = IstmoRuntime
        val ok = runtime.start("data_store_demo")
        check(ok) { "IstmoRuntime.start() failed — pump did not initialise" }

        runtime.registerHandler(
            DataStoreDispatcher.PLUGIN_ID,
            DataStoreDispatcher(DataStoreFactoryImpl(this), DataStoreCodecsImpl()),
        )

        super.onCreate(savedInstanceState)
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoRuntime.shutdown()
    }
}

