package dev.istmo.plugins.webauth

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResultCallback
import androidx.activity.result.ActivityResultCaller
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.ActivityResultRegistry
import androidx.activity.result.contract.ActivityResultContract
import androidx.browser.auth.AuthTabIntent
import androidx.browser.customtabs.CustomTabsClient
import androidx.browser.customtabs.CustomTabsIntent
import androidx.core.util.Consumer
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.IstmoActivityBound
import java.lang.ref.WeakReference
import java.security.SecureRandom
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.coroutines.resume
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout

/**
 * Reference backend for `istmo.web_auth`.
 *
 * Opens the authorization page in an Auth Tab when the default browser
 * supports one (Chrome 137+): the result comes straight back, no intent
 * filter involved. Otherwise it opens a Custom Tab and waits for the
 * redirect to reach the host activity (`onNewIntent`), which needs the
 * callback scheme declared in `[[app.url_scheme]]` (or a verified App
 * Link for `https`). Returning to the activity without a callback means
 * the user closed the tab.
 */
class WebAuthBackendImpl(activity: ComponentActivity) : WebAuthBackend, IstmoActivityBound {
    @Volatile
    private var activityRef = WeakReference(activity)

    private val prepared = HashMap<ULong, Prepared>()
    private val busy = AtomicBoolean(false)
    private val random = SecureRandom()

    private class Prepared(val spec: CallbackSpec, val redirect: Uri)

    override fun attachActivity(activity: Activity) {
        activityRef = WeakReference(activity as ComponentActivity)
    }

    override suspend fun capabilities(): WebAuthCapabilities {
        val activity = activityRef.get()
        val pkg = activity?.let { CustomTabsClient.getPackageName(it, null) }
        val authTab = activity != null && pkg != null && CustomTabsClient.isAuthTabSupported(activity, pkg)
        return WebAuthCapabilities(
            backend = if (authTab) "auth-tab" else "custom-tabs",
            loopbackCallbacks = false,
            schemeCallbacks = true,
            httpsCallbacks = true,
            ephemeral = authTab,
            embedded = false,
        )
    }

    override suspend fun prepare(callback: CallbackSpec): PreparedRedirect {
        val uri = when (callback) {
            is CallbackSpec.Loopback ->
                throw BackendException(WebAuthError.Unsupported("loopback callbacks on Android; use a custom scheme or https"))
            is CallbackSpec.CustomScheme -> {
                val spec = callback.value
                val scheme = spec.scheme.lowercase()
                if (!isValidScheme(scheme) || (spec.path.isNotEmpty() && !spec.path.startsWith("/"))) {
                    throw BackendException(WebAuthError.InvalidArgument("callback scheme"))
                }
                val path = spec.path.ifEmpty { "/" }
                if (spec.host.isEmpty()) "$scheme:$path" else "$scheme://${spec.host.lowercase()}$path"
            }
            is CallbackSpec.Https -> {
                val spec = callback.value
                "https://${spec.host.lowercase()}${spec.path.ifEmpty { "/" }}"
            }
        }
        val session = synchronized(prepared) {
            var id = random.nextLong().toULong()
            while (id == 0UL || prepared.containsKey(id)) {
                id = random.nextLong().toULong()
            }
            prepared[id] = Prepared(callback, Uri.parse(uri))
            id
        }
        return PreparedRedirect(redirectUri = uri, session = session)
    }

    override suspend fun authenticate(request: AuthRequest): AuthCallback {
        if (request.mode != AuthMode.System) {
            throw BackendException(WebAuthError.Unsupported("embedded sign-in on Android; Auth Tab / Custom Tabs are the supported path"))
        }
        val authorize = Uri.parse(request.authorizeUrl)
        if (authorize.scheme != "https") {
            throw BackendException(WebAuthError.InvalidArgument("authorize_url must be https"))
        }
        if (!busy.compareAndSet(false, true)) {
            throw BackendException(WebAuthError.AlreadyInProgress)
        }
        try {
            val entry = synchronized(prepared) { prepared.remove(request.session) }
                ?: throw BackendException(WebAuthError.UnknownSession)
            val run: suspend () -> Uri = {
                withContext(Dispatchers.Main) { present(authorize, entry, request) }
            }
            val uri = try {
                if (request.timeoutMs > 0U) withTimeout(request.timeoutMs.toLong()) { run() } else run()
            } catch (_: TimeoutCancellationException) {
                throw BackendException(WebAuthError.Timeout)
            }
            if (uri.getQueryParameter("state") != request.expectedState &&
                fragmentParameter(uri, "state") != request.expectedState
            ) {
                throw BackendException(WebAuthError.StateMismatch)
            }
            return AuthCallback(url = uri.toString())
        } finally {
            busy.set(false)
        }
    }

    private suspend fun present(authorize: Uri, entry: Prepared, request: AuthRequest): Uri {
        val activity = activityRef.get()
            ?: throw BackendException(WebAuthError.PresentationUnavailable)
        val browser = CustomTabsClient.getPackageName(activity, null)
            ?: browserPackage(activity, authorize)
            ?: throw BackendException(WebAuthError.NoBrowser)
        return if (CustomTabsClient.isAuthTabSupported(activity, browser)) {
            authTab(activity, authorize, entry, request.ephemeral)
        } else {
            customTab(activity, browser, authorize, entry, request.expectedState)
        }
    }

    /** Auth Tab: the browser returns the redirect as an activity result. */
    private suspend fun authTab(
        activity: ComponentActivity,
        authorize: Uri,
        entry: Prepared,
        ephemeral: Boolean,
    ): Uri = suspendCancellableCoroutine { cont ->
        lateinit var launcher: ActivityResultLauncher<Intent>
        val caller = RegistryCaller(activity.activityResultRegistry, "istmo.web_auth.${System.nanoTime()}")
        launcher = AuthTabIntent.registerActivityResultLauncher(caller) { result ->
            launcher.unregister()
            if (!cont.isActive) return@registerActivityResultLauncher
            when (result.resultCode) {
                AuthTabIntent.RESULT_OK -> result.resultUri?.let { cont.resume(it) }
                    ?: cont.fail(WebAuthError.Backend("Auth Tab returned no URI"))
                AuthTabIntent.RESULT_VERIFICATION_FAILED,
                AuthTabIntent.RESULT_VERIFICATION_TIMED_OUT -> cont.fail(WebAuthError.CallbackNotRegistered)
                else -> cont.fail(WebAuthError.Cancelled)
            }
        }
        cont.invokeOnCancellation { activity.runOnUiThread { launcher.unregister() } }
        val intent = AuthTabIntent.Builder().setEphemeralBrowsingEnabled(ephemeral).build()
        val redirect = entry.redirect
        try {
            if (redirect.scheme == "https") {
                intent.launch(launcher, authorize, redirect.host ?: "", redirect.path ?: "/")
            } else {
                intent.launch(launcher, authorize, redirect.scheme ?: "")
            }
        } catch (_: ActivityNotFoundException) {
            launcher.unregister()
            cont.fail(WebAuthError.NoBrowser)
        }
    }

    /**
     * Custom Tab: the redirect arrives as a new intent of the host
     * activity. Coming back to the activity without one means the tab
     * was closed.
     */
    private suspend fun customTab(
        activity: ComponentActivity,
        browser: String,
        authorize: Uri,
        entry: Prepared,
        state: String,
    ): Uri {
        val redirect = entry.redirect
        if (redirect.scheme != "https" && !declaresScheme(activity, redirect)) {
            throw BackendException(WebAuthError.CallbackNotRegistered)
        }
        var listener: Consumer<Intent>? = null
        var observer: DefaultLifecycleObserver? = null
        // Runs on the main dispatcher: listeners are added and removed on
        // the thread that delivers them.
        try {
            return suspendCancellableCoroutine { cont ->
                listener = Consumer<Intent> { intent ->
                    val uri = intent.data ?: return@Consumer
                    if (cont.isActive && matches(uri, redirect) &&
                        (uri.getQueryParameter("state") == state || fragmentParameter(uri, "state") == state)
                    ) {
                        cont.resume(uri)
                    }
                }.also(activity::addOnNewIntentListener)
                observer = object : DefaultLifecycleObserver {
                    private var left = false

                    override fun onStop(owner: LifecycleOwner) {
                        left = true
                    }

                    override fun onResume(owner: LifecycleOwner) {
                        if (!left) return
                        // `onNewIntent` runs before `onResume`; give a late
                        // redirect one frame before calling it a cancel.
                        activity.window.decorView.post {
                            if (cont.isActive) cont.fail(WebAuthError.Cancelled)
                        }
                    }
                }.also(activity.lifecycle::addObserver)
                val tab = CustomTabsIntent.Builder().build()
                tab.intent.setPackage(browser)
                try {
                    tab.launchUrl(activity, authorize)
                } catch (_: ActivityNotFoundException) {
                    cont.fail(WebAuthError.NoBrowser)
                }
            }
        } finally {
            listener?.let(activity::removeOnNewIntentListener)
            observer?.let(activity.lifecycle::removeObserver)
        }
    }


    private fun CancellableContinuation<Uri>.fail(error: WebAuthError) {
        if (isActive) resumeWith(Result.failure(BackendException(error)))
    }

    private fun matches(uri: Uri, redirect: Uri): Boolean =
        uri.scheme.equals(redirect.scheme, ignoreCase = true) &&
            (uri.host ?: "").equals(redirect.host ?: "", ignoreCase = true) &&
            (uri.path ?: "").ifEmpty { "/" } == (redirect.path ?: "").ifEmpty { "/" }

    private fun declaresScheme(activity: Activity, redirect: Uri): Boolean {
        val probe = Intent(Intent.ACTION_VIEW, redirect).addCategory(Intent.CATEGORY_BROWSABLE)
            .setPackage(activity.packageName)
        return activity.packageManager.queryIntentActivities(probe, 0).isNotEmpty()
    }

    private fun browserPackage(activity: Activity, url: Uri): String? {
        val view = Intent(Intent.ACTION_VIEW, url).addCategory(Intent.CATEGORY_BROWSABLE)
        return activity.packageManager
            .resolveActivity(view, PackageManager.MATCH_DEFAULT_ONLY)
            ?.activityInfo?.packageName
    }

    private fun fragmentParameter(uri: Uri, name: String): String? =
        uri.encodedFragment?.let { Uri.parse("x:/?$it").getQueryParameter(name) }

    private fun isValidScheme(scheme: String): Boolean =
        scheme.isNotEmpty() && scheme[0] in 'a'..'z' &&
            scheme.all { it in 'a'..'z' || it in '0'..'9' || it in "+-." } &&
            scheme !in setOf("http", "https", "file", "javascript", "data", "intent", "content")
}

/**
 * Registers on the activity's result registry by key: unlike
 * `registerForActivityResult`, that works after the activity started,
 * when a sign-in is requested.
 */
private class RegistryCaller(
    private val registry: ActivityResultRegistry,
    private val key: String,
) : ActivityResultCaller {
    override fun <I, O> registerForActivityResult(
        contract: ActivityResultContract<I, O>,
        callback: ActivityResultCallback<O>,
    ): ActivityResultLauncher<I> = registry.register(key, contract, callback)

    override fun <I, O> registerForActivityResult(
        contract: ActivityResultContract<I, O>,
        registry: ActivityResultRegistry,
        callback: ActivityResultCallback<O>,
    ): ActivityResultLauncher<I> = registry.register(key, contract, callback)
}
