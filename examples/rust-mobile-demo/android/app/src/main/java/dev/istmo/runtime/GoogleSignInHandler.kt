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
import java.io.ByteArrayOutputStream
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.Continuation
import kotlin.coroutines.resume
import kotlin.coroutines.suspendCoroutine
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.tasks.await
import kotlinx.coroutines.withContext

/**
 * Kotlin backend for the `istmo.google_sign_in` plugin, backed by the
 * legacy `GoogleSignInClient` (from `com.google.android.gms:play-services-auth`).
 *
 * Legacy vs Credential Manager: the M6 spec pointed at Credential Manager
 * for future-proofing, but on MIUI / other aggressive process managers
 * every Credential-Manager path goes through a Play-Services-launched
 * intermediate Activity that MIUI kills mid-flow, surfacing as a bogus
 * `TYPE_USER_CANCELED` right after account selection. The legacy client
 * launches the picker directly from the calling Activity via
 * `startActivityForResult`, which MIUI leaves alone.
 *
 * Trait shape and wire encoding are unchanged — the swap is
 * dispatcher-local.
 */
@Suppress("DEPRECATION") // legacy GoogleSignInClient — see MIUI note above.
class GoogleSignInHandler(private val activity: Activity) : PluginHandler {

    companion object {
        const val PLUGIN_ID = "istmo.google_sign_in"
        private const val TAG = "istmo.signin"
        /**
         * Base request code for the sign-in `startActivityForResult` flow.
         * Each concurrent request gets `base + n`; the demo only ever
         * fires one, so `base` alone would be enough, but the counter
         * keeps things obvious if a future refactor adds parallelism.
         */
        private const val REQUEST_CODE_BASE = 0xE00
    }

    private val nextInstanceId = AtomicLong(1)
    private val nextHandleId = AtomicLong(1)
    private val nextRequestOffset = AtomicInteger(0)
    private val configs = ConcurrentHashMap<Long, SignInConfig>()
    private val credentials = ConcurrentHashMap<Long, GoogleSignInAccount>()
    private val pending = ConcurrentHashMap<Int, Continuation<SignInResult>>()

    override suspend fun handleCreateInstance(payload: ByteArray): ByteArray {
        val config = decodeConfig(payload)
        val instanceId = nextInstanceId.getAndIncrement()
        configs[instanceId] = config
        val out = ByteArrayOutputStream(4)
        Bincode.writeVarintU64(out, instanceId)
        return out.toByteArray()
    }

    override suspend fun handleCall(
        instanceId: Long,
        method: String,
        payload: ByteArray,
    ): ByteArray {
        val config = configs[instanceId]
            ?: throw PluginException(encodeError(Err.Backend, "unknown instance id $instanceId"))

        return when (method) {
            "sign_in" -> {
                val (mode, _) = Bincode.readEnumDiscriminant(payload, 0)
                val account = when (mode) {
                    0 -> interactiveSignIn(config)  // Interactive
                    1 -> silentSignIn(config)       // SilentOnly
                    else -> throw PluginException(
                        encodeError(Err.Backend, "unknown SignInMode discriminant $mode"),
                    )
                }
                encodeAccount(account)
            }
            "silent_sign_in" -> {
                val account = try {
                    silentSignIn(config)
                } catch (e: PluginException) {
                    if (isNoCredential(e)) return encodeOptionNone()
                    throw e
                }
                encodeOptionSome(account)
            }
            "refresh" -> {
                val (handleId, _) = Bincode.readVarintU64(payload, 0)
                credentials.remove(handleId)
                encodeAccount(silentSignIn(config))
            }
            "sign_out" -> {
                credentials.clear()
                clientFor(config).signOut().await()
                ByteArray(0)
            }
            "revoke" -> {
                credentials.clear()
                clientFor(config).revokeAccess().await()
                ByteArray(0)
            }
            else -> error("unknown SignIn method: $method")
        }
    }

    /**
     * Route from [RustMobileActivity.onActivityResult] into the suspended
     * `startActivityForResult` continuation.
     */
    fun notifyActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        val cont = pending.remove(requestCode) ?: return
        cont.resume(SignInResult(resultCode, data))
    }

    private suspend fun interactiveSignIn(config: SignInConfig): SignInAccount {
        val client = clientFor(config)
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

    private suspend fun silentSignIn(config: SignInConfig): SignInAccount {
        val client = clientFor(config)
        return try {
            toAccount(client.silentSignIn().await())
        } catch (e: ApiException) {
            Log.w(
                TAG,
                "silent sign-in failed status=${e.statusCode} ${CommonStatusCodes.getStatusCodeString(e.statusCode)}",
                e,
            )
            throw PluginException(encodeError(Err.NoCredentialAvailable, null))
        }
    }

    private fun resolveResult(result: SignInResult): SignInAccount {
        val data = result.data
        val task = GoogleSignIn.getSignedInAccountFromIntent(data)
        return try {
            val account = task.getResult(ApiException::class.java)
                ?: throw PluginException(encodeError(Err.Backend, "empty sign-in result"))
            Log.i(TAG, "picker sign-in succeeded id=${account.id} email=${account.email}")
            toAccount(account)
        } catch (e: ApiException) {
            val code = e.statusCode
            val label = CommonStatusCodes.getStatusCodeString(code)
            Log.e(TAG, "picker sign-in failed status=$code $label", e)
            val err = when (code) {
                CommonStatusCodes.SIGN_IN_REQUIRED,
                GoogleSignInStatusCodes.SIGN_IN_CANCELLED -> Err.UserCancelled
                CommonStatusCodes.NETWORK_ERROR -> Err.Network
                CommonStatusCodes.DEVELOPER_ERROR -> Err.InvalidConfiguration
                else -> Err.Backend
            }
            val hint = when (code) {
                CommonStatusCodes.DEVELOPER_ERROR ->
                    " — Google Cloud Console: verify (a) an Android OAuth client with package " +
                        "`${activity.packageName}` + SHA-1 of the APK signing keystore exists, " +
                        "AND (b) the SERVER_CLIENT_ID Web client is in the SAME project. " +
                        "Run `just helper` on the host to see the SHA-1."
                else -> ""
            }
            val msg = if (err.hasPayload) "$label ($code)$hint" else null
            throw PluginException(encodeError(err, msg))
        }
    }

    private fun clientFor(config: SignInConfig): GoogleSignInClient {
        val builder = GoogleSignInOptions.Builder(GoogleSignInOptions.DEFAULT_SIGN_IN)
            .requestIdToken(config.serverClientId)
            .requestEmail()
            .requestProfile()
        for (scope in config.scopes) {
            // Skip the OIDC-standard scopes the DEFAULT_SIGN_IN + requestEmail + requestProfile
            // already cover, otherwise Google logs a warning about duplicates.
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
        val handleId = nextHandleId.getAndIncrement()
        credentials[handleId] = google
        return SignInAccount(
            id = google.id ?: "",
            email = google.email,
            displayName = google.displayName,
            photoUrl = google.photoUrl?.toString(),
            idToken = google.idToken ?: "",
            grantedScopes = google.grantedScopes.map { it.scopeUri },
            credentialHandleId = handleId,
        )
    }

    fun releaseCredential(handleId: Long) {
        credentials.remove(handleId)
    }

    private fun isNoCredential(e: PluginException): Boolean {
        val bytes = e.payload
        if (bytes.isEmpty()) return false
        return Bincode.readEnumDiscriminant(bytes, 0).value ==
            Err.NoCredentialAvailable.ordinal
    }

    // ---- Bincode -------------------------------------------------------

    /**
     * `SignInConfig` wire layout — see the Rust plugin for the canonical
     * definition:
     *   String serverClientId
     *   Vec<String> scopes
     *   Option<String> hostedDomain
     *   Option<String> nonce
     *   bool autoSelect
     */
    private fun decodeConfig(payload: ByteArray): SignInConfig {
        var cursor = 0
        val (server, c1) = Bincode.readString(payload, cursor); cursor = c1
        val (scopes, c2) = Bincode.readVec(payload, cursor, Bincode::readString); cursor = c2
        val (hosted, c3) = Bincode.readOption(payload, cursor, Bincode::readString); cursor = c3
        val (nonce, c4) = Bincode.readOption(payload, cursor, Bincode::readString); cursor = c4
        val (auto, _) = Bincode.readBool(payload, cursor)
        return SignInConfig(server, scopes, hosted, nonce, auto)
    }

    /**
     * `SignInAccount` wire layout:
     *   String id
     *   Option<String> email
     *   Option<String> displayName
     *   Option<String> photoUrl
     *   String idToken
     *   Vec<String> grantedScopes
     *   NativeHandleId credential      (u64 varint)
     */
    private fun encodeAccount(account: SignInAccount): ByteArray {
        val out = ByteArrayOutputStream()
        Bincode.writeString(out, account.id)
        Bincode.writeOption(out, account.email, Bincode::writeString)
        Bincode.writeOption(out, account.displayName, Bincode::writeString)
        Bincode.writeOption(out, account.photoUrl, Bincode::writeString)
        Bincode.writeString(out, account.idToken)
        Bincode.writeVec(out, account.grantedScopes, Bincode::writeString)
        Bincode.writeVarintU64(out, account.credentialHandleId)
        return out.toByteArray()
    }

    private fun encodeOptionNone(): ByteArray = byteArrayOf(0)

    private fun encodeOptionSome(account: SignInAccount): ByteArray {
        val out = ByteArrayOutputStream()
        out.write(1)
        out.write(encodeAccount(account))
        return out.toByteArray()
    }

    /**
     * `SignInError`:
     *   0 UserCancelled              (unit)
     *   1 NoCredentialAvailable      (unit)
     *   2 Reauthenticate             (unit)
     *   3 InvalidConfiguration(String)
     *   4 Network(String)
     *   5 Backend(String)
     */
    private fun encodeError(err: Err, message: String?): ByteArray {
        val out = ByteArrayOutputStream()
        Bincode.writeEnumDiscriminant(out, err.ordinal)
        if (err.hasPayload) {
            Bincode.writeString(out, message ?: "")
        }
        return out.toByteArray()
    }

    private enum class Err(val hasPayload: Boolean) {
        UserCancelled(false),
        NoCredentialAvailable(false),
        Reauthenticate(false),
        InvalidConfiguration(true),
        Network(true),
        Backend(true),
    }

    private data class SignInConfig(
        val serverClientId: String,
        val scopes: List<String>,
        val hostedDomain: String?,
        val nonce: String?,
        val autoSelect: Boolean,
    )

    private data class SignInAccount(
        val id: String,
        val email: String?,
        val displayName: String?,
        val photoUrl: String?,
        val idToken: String,
        val grantedScopes: List<String>,
        val credentialHandleId: Long,
    )

    private data class SignInResult(val resultCode: Int, val data: Intent?)
}

