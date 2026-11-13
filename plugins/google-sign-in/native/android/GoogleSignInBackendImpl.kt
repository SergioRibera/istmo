package dev.istmo.plugins.googlesignin

import android.app.Activity
import android.util.Base64
import androidx.credentials.ClearCredentialStateRequest
import androidx.credentials.CredentialManager
import androidx.credentials.CredentialOption
import androidx.credentials.CustomCredential
import androidx.credentials.GetCredentialRequest
import androidx.credentials.exceptions.ClearCredentialException
import androidx.credentials.exceptions.GetCredentialCancellationException
import androidx.credentials.exceptions.GetCredentialException
import androidx.credentials.exceptions.GetCredentialInterruptedException
import androidx.credentials.exceptions.NoCredentialException
import com.google.android.libraries.identity.googleid.GetGoogleIdOption
import com.google.android.libraries.identity.googleid.GetSignInWithGoogleOption
import com.google.android.libraries.identity.googleid.GoogleIdTokenCredential
import com.google.android.libraries.identity.googleid.GoogleIdTokenParsingException
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.HandleReleaser
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.NativeHandleId
import java.util.concurrent.ConcurrentHashMap
import org.json.JSONObject

/**
 * Reference Credential Manager backend for `istmo.google_sign_in`.
 *
 * * `SignInMode.Interactive` shows the "Sign in with Google" sheet
 *   (`GetSignInWithGoogleOption`).
 * * `SignInMode.SilentOnly` / `silent_sign_in` ask only for accounts that
 *   already authorised this app, with auto-select. Credential Manager has
 *   no fully headless mode: with exactly one such account the system
 *   still flashes a short "signing you in" banner.
 *
 * Limits of Credential Manager that callers should know about:
 *
 * * It authenticates; it does not grant OAuth scopes. Extra
 *   `SignInConfig.scopes` need the separate Authorization API, so
 *   `grantedScopes` only ever lists `openid`, `email` and `profile`.
 * * There is no token refresh: `refresh` runs the silent flow again.
 * * There is no revoke call: `revoke` clears the local credential state;
 *   revoking the grant on Google's side is a call to
 *   `https://oauth2.googleapis.com/revoke` from your server.
 */
class GoogleSignInBackendImpl(
    private val activity: Activity,
    private val config: SignInConfig,
) : SignInBackend, HandleReleaser {

    private val credentialManager = CredentialManager.create(activity)
    private val credentials = ConcurrentHashMap<Long, GoogleIdTokenCredential>()

    override suspend fun sign_in(mode: SignInMode): SignInAccount = when (mode) {
        SignInMode.Interactive -> request(interactiveOption())
        SignInMode.SilentOnly -> request(silentOption())
    }

    override suspend fun silent_sign_in(): SignInAccount? = try {
        request(silentOption())
    } catch (e: BackendException) {
        if (e.error == SignInError.NoCredentialAvailable) null else throw e
    }

    override suspend fun refresh(credential: NativeHandleId): SignInAccount {
        if (credentials.remove(credential) == null) {
            throw BackendException(SignInError.Reauthenticate)
        }
        IstmoRuntime.forgetHandle(credential)
        return try {
            request(silentOption())
        } catch (e: BackendException) {
            if (e.error == SignInError.NoCredentialAvailable) {
                throw BackendException(SignInError.Reauthenticate)
            }
            throw e
        }
    }

    override suspend fun sign_out() {
        clearState()
    }

    override suspend fun revoke() {
        clearState()
    }

    override fun releaseNativeHandle(handleId: Long) {
        if (credentials.remove(handleId) != null) IstmoRuntime.forgetHandle(handleId)
    }

    private fun interactiveOption(): CredentialOption =
        GetSignInWithGoogleOption.Builder(config.serverClientId)
            .apply {
                config.nonce?.let { setNonce(it) }
                config.hostedDomain?.let { setHostedDomainFilter(it) }
            }
            .build()

    private fun silentOption(): CredentialOption =
        GetGoogleIdOption.Builder()
            .setServerClientId(config.serverClientId)
            .setFilterByAuthorizedAccounts(true)
            .setAutoSelectEnabled(true)
            .apply { config.nonce?.let { setNonce(it) } }
            .build()

    private suspend fun request(option: CredentialOption): SignInAccount {
        val request = GetCredentialRequest.Builder().addCredentialOption(option).build()
        val response = try {
            credentialManager.getCredential(activity, request)
        } catch (e: GetCredentialCancellationException) {
            throw BackendException(SignInError.UserCancelled)
        } catch (e: NoCredentialException) {
            throw BackendException(SignInError.NoCredentialAvailable)
        } catch (e: GetCredentialInterruptedException) {
            throw BackendException(SignInError.Network(e.message ?: "interrupted"))
        } catch (e: GetCredentialException) {
            throw BackendException(SignInError.Backend("${e.type}: ${e.message}"))
        }
        val credential = response.credential
        if (credential !is CustomCredential ||
            credential.type != GoogleIdTokenCredential.TYPE_GOOGLE_ID_TOKEN_CREDENTIAL
        ) {
            throw BackendException(SignInError.Backend("unexpected credential type ${credential.type}"))
        }
        val google = try {
            GoogleIdTokenCredential.createFrom(credential.data)
        } catch (e: GoogleIdTokenParsingException) {
            throw BackendException(SignInError.Backend("parse Google id token: ${e.message}"))
        }
        return toAccount(google)
    }

    private suspend fun clearState() {
        try {
            credentialManager.clearCredentialState(ClearCredentialStateRequest())
        } catch (e: ClearCredentialException) {
            throw BackendException(SignInError.Backend("clear credential state: ${e.message}"))
        }
        for (id in credentials.keys) {
            credentials.remove(id)
            IstmoRuntime.forgetHandle(id)
        }
    }

    private fun toAccount(google: GoogleIdTokenCredential): SignInAccount {
        val claims = idTokenClaims(google.idToken)
        // `GoogleIdTokenCredential.id` is the email address; the stable
        // account id is the token's `sub` claim.
        val subject = claims?.optString("sub").orEmpty()
        if (subject.isEmpty()) {
            throw BackendException(SignInError.Backend("id token carries no `sub` claim"))
        }
        val handleId = IstmoRuntime.allocHandleId(SignInDispatcher.PLUGIN_ID)
        credentials[handleId] = google
        return SignInAccount(
            id = subject,
            email = claims?.optString("email")?.takeIf { it.isNotEmpty() } ?: google.id,
            displayName = google.displayName,
            photoUrl = google.profilePictureUri?.toString(),
            idToken = google.idToken,
            grantedScopes = listOf("openid", "email", "profile"),
            credential = handleId,
        )
    }

    /**
     * The JWT payload, decoded without verifying the signature: only
     * display claims are read here. Your server must verify the token.
     */
    private fun idTokenClaims(idToken: String): JSONObject? {
        val payload = idToken.split('.').getOrNull(1) ?: return null
        return try {
            val json = Base64.decode(payload, Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP)
            JSONObject(String(json, Charsets.UTF_8))
        } catch (e: IllegalArgumentException) {
            null
        } catch (e: org.json.JSONException) {
            null
        }
    }
}
