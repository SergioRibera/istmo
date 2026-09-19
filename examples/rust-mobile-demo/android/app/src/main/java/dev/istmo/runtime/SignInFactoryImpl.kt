package dev.istmo.runtime

import android.app.Activity

class SignInFactoryImpl(private val activity: Activity) : SignInFactory {

    private var last: SignInBackendImpl? = null

    override suspend fun create(config: SignInConfig): SignInBackend {
        val backend = SignInBackendImpl(activity, config)
        last = backend
        return backend
    }

    fun rememberLastBackend(): SignInBackendImpl? = last
}

