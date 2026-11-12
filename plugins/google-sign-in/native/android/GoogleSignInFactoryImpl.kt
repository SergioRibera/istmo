package dev.istmo.plugins.googlesignin

import android.app.Activity
import dev.istmo.runtime.SignInBackend
import dev.istmo.runtime.SignInConfig
import dev.istmo.runtime.SignInFactory

/**
 * One [GoogleSignInBackendImpl] per `SignInClient` instance. Credential
 * Manager shows its sheets over an `Activity`, so the generated
 * registry passes the host activity (`android_backend_arg = "activity"`).
 */
class GoogleSignInFactoryImpl(private val activity: Activity) : SignInFactory {
    override suspend fun create(config: SignInConfig): SignInBackend =
        GoogleSignInBackendImpl(activity, config)
}
