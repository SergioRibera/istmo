package dev.istmo.runtime

import android.app.Activity

/**
 * Instance factory for the stateful `SignIn` plugin.
 *
 * `SignInDispatcher.handleCreateInstance` decodes the `SignInConfig`
 * payload the caller shipped through `SignInClient::acquire_with`, then
 * calls this factory to allocate the per-instance backend. The factory
 * itself is cheap (no SDK init required at construction time — the
 * `GoogleSignInClient` is built on-demand per call).
 *
 * The `Activity` reference is captured so the backend can drive
 * `startActivityForResult`. The `RustMobileActivity` keeps a reference
 * to the *last-created* backend so `onActivityResult` can forward
 * result callbacks — see [rememberLastBackend].
 */
class SignInFactoryImpl(private val activity: Activity) : SignInFactory {

    private var last: SignInBackendImpl? = null

    override suspend fun create(config: SignInConfig): SignInBackend {
        val backend = SignInBackendImpl(activity, config)
        last = backend
        return backend
    }

    /** Latest backend the factory created — the demo Activity forwards
     *  `onActivityResult` into it. `null` before the first `acquire_with`. */
    fun rememberLastBackend(): SignInBackendImpl? = last
}
