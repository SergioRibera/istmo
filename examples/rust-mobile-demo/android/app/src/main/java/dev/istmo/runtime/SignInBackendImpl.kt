package dev.istmo.runtime

import android.app.Activity
import android.content.Intent
import android.util.Log
import com.google.android.gms.auth.api.signin.GoogleSignIn
import com.google.android.gms.auth.api.signin.GoogleSignInAccount
import com.google.android.gms.auth.api.signin.GoogleSignInClient
import com.google.android.gms.auth.api.signin.GoogleSignInOptions
import com.google.android.gms.auth.api.signin.GoogleSignInStatusCodes
import com.google.android.gms.common.Scopes
import com.google.android.gms.common.api.ApiException
import com.google.android.gms.common.api.CommonStatusCodes
import com.google.android.gms.common.api.Scope
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger
import kotlin.coroutines.Continuation
import kotlin.coroutines.resume
import kotlin.coroutines.suspendCoroutine
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.tasks.await
import kotlinx.coroutines.withContext

/**
 * Android impl of the codegen `SignInBackend` interface, backed by the
 * legacy `GoogleSignInClient` (`play-services-auth`).
 *
 * Legacy vs Credential Manager — the M6 spec pointed at Credential
 * Manager, but on MIUI every Credential-Manager path goes through a
 * Play-Services-launched intermediate Activity that MIUI kills mid-flow,
 * surfacing as a bogus `TYPE_USER_CANCELED` right after account
 * selection. The legacy client launches the picker directly from the
 * calling Activity via `startActivityForResult`, which MIUI leaves alone.
 *
 * `notifyActivityResult` forwards the Activity's result callback into
 * the suspended sign-in coroutine.
 */
@Suppress("DEPRECATION") // legacy GoogleSignInClient — see MIUI note above.
class SignInBackendImpl(private val activity: Activity, private val config: SignInConfig) :
    SignInBackend, HandleReleaser {

    companion object {
        private const val TAG = "istmo.signin"
        /**
         * Base request code for the sign-in `startActivityForResult` flow.
         * Each concurrent request gets `base + n`; the demo only ever
         * fires one, so `base` alone would be enough — the counter keeps
         * things obvious if a future refactor adds parallelism.
         */
        private const val REQUEST_CODE_BASE = 0xE00
    }

    private val nextRequestOffset = AtomicInteger(0)
    private val credentials = ConcurrentHashMap<Long, GoogleSignInAccount>()
    private val pending = ConcurrentHashMap<Int, Continuation<SignInResult>>()

    override fun releaseNativeHandle(handleId: Long) {
        credentials.remove(handleId)
    }

    override suspend fun sign_in(mode: SignInMode): SignInAccount = when (mode) {
        SignInMode.Interactive -> interactiveSignIn()
        SignInMode.SilentOnly -> silentSignIn()
    }

    override suspend fun silent_sign_in(): SignInAccount? {
        return try {
            silentSignIn()
        } catch (e: BackendException) {
            if (e.error === SignInError.NoCredentialAvailable) null else throw e
        }
    }

    override suspend fun refresh(credential: NativeHandleId): SignInAccount {
        credentials.remove(credential)
        return silentSignIn()
    }

    override suspend fun sign_out() {
        credentials.clear()
        client().signOut().await()
    }

    override suspend fun revoke() {
        credentials.clear()
        client().revokeAccess().await()
    }

    /** Route from `RustMobileActivity.onActivityResult` into the
     *  suspended `startActivityForResult` continuation. */
    fun notifyActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        val cont = pending.remove(requestCode) ?: return
        cont.resume(SignInResult(resultCode, data))
    }

    // ---- Flows -----------------------------------------------------------

    private suspend fun interactiveSignIn(): SignInAccount {
        val client = client()
        // Try silent first — instantaneous when the user already granted
        // this app the requested scopes.
        try {
            val silent = client.silentSignIn().await()
            Log.i(TAG, "silent sign-in succeeded id=${silent.id}")
            return toAccount(silent)
        } catch (e: ApiException) {
            Log.i(
                TAG,
                "silent sign-in unavailable (status=${e.statusCode} ${CommonStatusCodes.getStatusCodeString(e.statusCode)}) — launching picker",
            )
        }

        val requestCode = REQUEST_CODE_BASE + (nextRequestOffset.getAndIncrement() and 0xFF)
        val intent = client.signInIntent
        val result: SignInResult = withContext(Dispatchers.Main) {
            suspendCoroutine { cont ->
                pending[requestCode] = cont
                activity.startActivityForResult(intent, requestCode)
            }
        }
        return resolveResult(result)
    }

    private suspend fun silentSignIn(): SignInAccount {
        val client = client()
        return try {
            toAccount(client.silentSignIn().await())
        } catch (e: ApiException) {
            Log.w(
                TAG,
                "silent sign-in failed status=${e.statusCode} ${CommonStatusCodes.getStatusCodeString(e.statusCode)}",
                e,
            )
            throw BackendException(SignInError.NoCredentialAvailable)
        }
    }

    private fun resolveResult(result: SignInResult): SignInAccount {
        val task = GoogleSignIn.getSignedInAccountFromIntent(result.data)
        return try {
            val account = task.getResult(ApiException::class.java)
                ?: throw BackendException(SignInError.Backend("empty sign-in result"))
            Log.i(TAG, "picker sign-in succeeded id=${account.id} email=${account.email}")
            toAccount(account)
        } catch (e: ApiException) {
            val code = e.statusCode
            val label = CommonStatusCodes.getStatusCodeString(code)
            Log.e(TAG, "picker sign-in failed status=$code $label", e)
            val err: SignInError = when (code) {
                CommonStatusCodes.SIGN_IN_REQUIRED,
                GoogleSignInStatusCodes.SIGN_IN_CANCELLED -> SignInError.UserCancelled
                CommonStatusCodes.NETWORK_ERROR -> SignInError.Network("$label ($code)")
                CommonStatusCodes.DEVELOPER_ERROR -> {
                    val hint = " — Google Cloud Console: verify (a) an Android OAuth client " +
                        "with package `${activity.packageName}` + SHA-1 of the APK signing " +
                        "keystore exists, AND (b) the SERVER_CLIENT_ID Web client is in the " +
                        "SAME project. Run `just helper` on the host to see the SHA-1."
                    SignInError.InvalidConfiguration("$label ($code)$hint")
                }
                else -> SignInError.Backend("$label ($code)")
            }
            throw BackendException(err)
        }
    }

    private fun client(): GoogleSignInClient {
        val builder = GoogleSignInOptions.Builder(GoogleSignInOptions.DEFAULT_SIGN_IN)
            .requestIdToken(config.serverClientId)
            .requestEmail()
            .requestProfile()
        for (scope in config.scopes) {
            // Skip the OIDC-standard scopes DEFAULT_SIGN_IN + requestEmail +
            // requestProfile already cover — Google logs a warning on duplicates.
            when (scope) {
                "openid", "email", "profile" -> continue
                Scopes.EMAIL, Scopes.PROFILE, Scopes.OPEN_ID -> continue
                else -> builder.requestScopes(Scope(scope))
            }
        }
        if (!config.hostedDomain.isNullOrEmpty()) {
            builder.setHostedDomain(config.hostedDomain)
        }
        return GoogleSignIn.getClient(activity, builder.build())
    }

    private fun toAccount(google: GoogleSignInAccount): SignInAccount {
        val handleId = IstmoRuntime.allocHandleId(SignInDispatcher.PLUGIN_ID)
        credentials[handleId] = google
        return SignInAccount(
            id = google.id ?: "",
            email = google.email,
            displayName = google.displayName,
            photoUrl = google.photoUrl?.toString(),
            idToken = google.idToken ?: "",
            grantedScopes = google.grantedScopes.map { it.scopeUri },
            credential = handleId,
        )
    }

    private data class SignInResult(val resultCode: Int, val data: Intent?)
}
