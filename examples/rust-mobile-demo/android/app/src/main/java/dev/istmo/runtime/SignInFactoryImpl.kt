package dev.istmo.runtime

import android.app.Activity
import dev.istmo.plugins.googlesignin.SignInBackend
import dev.istmo.plugins.googlesignin.SignInConfig
import dev.istmo.plugins.googlesignin.SignInFactory

class SignInFactoryImpl(private val activity: Activity) : SignInFactory {

    private var last: SignInBackendImpl? = null

    override suspend fun create(config: SignInConfig): SignInBackend {
        val backend = SignInBackendImpl(activity, config)
        last = backend
        return backend
    }

    fun rememberLastBackend(): SignInBackendImpl? = last
}

