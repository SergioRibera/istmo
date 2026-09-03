package dev.istmo.runtime

import android.app.Activity
import androidx.credentials.CredentialManager
import androidx.credentials.GetCredentialRequest
import androidx.credentials.exceptions.GetCredentialCancellationException
import androidx.credentials.exceptions.GetCredentialException
import androidx.credentials.exceptions.NoCredentialException
import com.google.android.libraries.identity.googleid.GetGoogleIdOption
import com.google.android.libraries.identity.googleid.GoogleIdTokenCredential
import java.io.ByteArrayOutputStream
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/**
 * Kotlin backend for the `istmo.google_sign_in` plugin.
 *
 * Uses AndroidX Credential Manager + Google Identity Services. Wire trait:
 *  * `sign_in(SignInMode) -> Result<SignInAccount, SignInError>`
 *  * `silent_sign_in() -> Result<Option<SignInAccount>, SignInError>`
 *  * `refresh(NativeHandleId) -> Result<SignInAccount, SignInError>`
 *  * `sign_out() -> Result<(), SignInError>`
 *  * `revoke() -> Result<(), SignInError>`
 *
 * Every returned account carries a `NativeHandleId` (u64) that resolves in
 * the [credentials] map to the concrete `GoogleIdTokenCredential`. Rust
 * releases the handle by sending `Frame::ReleaseNativeHandle`; the runtime
 * routes that into [IstmoRuntime.onReleaseNativeHandle], from which we
 * evict the entry.
 *
 * This handler is stateful — the trait is annotated with `init = SignInConfig`,
 * so [handleCreateInstance] parses the config and returns an instance id.
 */
class GoogleSignInHandler(private val activity: Activity) : PluginHandler {

    companion object {
        const val PLUGIN_ID = "istmo.google_sign_in"
    }

    private val nextInstanceId = AtomicLong(1)
    private val nextHandleId = AtomicLong(1)
    private val configs = ConcurrentHashMap<Long, SignInConfig>()
    private val credentials = ConcurrentHashMap<Long, GoogleIdTokenCredential>()
    private val manager: CredentialManager = CredentialManager.create(activity)

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
                val silent = mode == 1 // SignInMode: 0 = Interactive, 1 = SilentOnly
                encodeAccount(getCredential(config, filterByAuthorizedAccounts = silent))
            }
            "silent_sign_in" -> {
                val account = try {
                    getCredential(config, filterByAuthorizedAccounts = true)
                } catch (e: PluginException) {
                    val payloadBytes = e.payload
                    if (payloadBytes.isNotEmpty() &&
                        Bincode.readEnumDiscriminant(payloadBytes, 0).value == Err.NoCredentialAvailable.ordinal
                    ) {
                        return encodeOptionNone()
                    }
                    throw e
                }
                encodeOptionSome(account)
            }
            "refresh" -> {
                val (handleId, _) = Bincode.readVarintU64(payload, 0)
                credentials.remove(handleId)  // discard the stale handle
                encodeAccount(getCredential(config, filterByAuthorizedAccounts = true))
            }
            "sign_out" -> {
                credentials.clear()
                ByteArray(0)
            }
            "revoke" -> {
                credentials.clear()
                // Full OAuth revocation would require a backend call; the
                // Credential Manager API does not expose a revoke primitive
                // as of 1.3.x, so this is a local-state clear only. Real
                // apps should call the backend's `/token/revoke` endpoint.
                ByteArray(0)
            }
            else -> error("unknown SignIn method: $method")
        }
    }

    private suspend fun getCredential(
        config: SignInConfig,
        filterByAuthorizedAccounts: Boolean,
    ): SignInAccount {
        val optionBuilder = GetGoogleIdOption.Builder()
            .setServerClientId(config.serverClientId)
            .setFilterByAuthorizedAccounts(filterByAuthorizedAccounts)
            .setAutoSelectEnabled(config.autoSelect)
        config.nonce?.let { optionBuilder.setNonce(it) }
        val request = GetCredentialRequest.Builder()
            .addCredentialOption(optionBuilder.build())
            .build()
        val response = try {
            manager.getCredential(activity, request)
        } catch (_: GetCredentialCancellationException) {
            throw PluginException(encodeError(Err.UserCancelled, null))
        } catch (_: NoCredentialException) {
            throw PluginException(encodeError(Err.NoCredentialAvailable, null))
        } catch (e: GetCredentialException) {
            throw PluginException(encodeError(Err.Backend, e.message ?: e.javaClass.simpleName))
        }
        val credential = response.credential
        if (credential !is androidx.credentials.CustomCredential ||
            credential.type != GoogleIdTokenCredential.TYPE_GOOGLE_ID_TOKEN_CREDENTIAL
        ) {
            throw PluginException(encodeError(Err.Backend, "non-google credential returned"))
        }
        val google = GoogleIdTokenCredential.createFrom(credential.data)
        val handleId = nextHandleId.getAndIncrement()
        credentials[handleId] = google
        return SignInAccount(
            id = google.id,
            email = google.id,
            displayName = google.displayName,
            photoUrl = google.profilePictureUri?.toString(),
            idToken = google.idToken,
            grantedScopes = listOf("openid", "email", "profile"),
            credentialHandleId = handleId,
        )
    }

    fun releaseCredential(handleId: Long) {
        credentials.remove(handleId)
    }

    // ---- Bincode -------------------------------------------------------

    /**
     * `SignInConfig` layout:
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
     * `SignInAccount` layout:
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
}
