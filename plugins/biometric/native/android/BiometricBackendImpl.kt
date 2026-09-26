package dev.istmo.plugins.biometric

import android.app.KeyguardManager
import android.content.Context
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.security.keystore.UserNotAuthenticatedException
import android.util.Base64
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricManager.Authenticators.BIOMETRIC_STRONG
import androidx.biometric.BiometricManager.Authenticators.BIOMETRIC_WEAK
import androidx.biometric.BiometricManager.Authenticators.DEVICE_CREDENTIAL
import androidx.biometric.BiometricPrompt
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity
import dev.istmo.runtime.AuthMethod
import dev.istmo.runtime.AuthPolicy
import dev.istmo.runtime.AuthPrompt
import dev.istmo.runtime.Availability
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.BiometricBackend
import dev.istmo.runtime.BiometricError
import dev.istmo.runtime.BiometricKind
import dev.istmo.runtime.BiometricStatus
import java.security.GeneralSecurityException
import java.security.KeyStore
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

/**
 * Reference `androidx.biometric` backend for `istmo.biometric`.
 *
 * `BiometricPrompt` attaches a headless fragment to its host, so the
 * backend needs a `FragmentActivity` (`AppCompatActivity`,
 * `GameActivity`, …) — a bare `NativeActivity` will not do. Construct it
 * in `onCreate` and register the codegen-emitted dispatcher:
 *
 * ```kotlin
 * override fun onCreate(savedInstanceState: Bundle?) {
 *     super.onCreate(savedInstanceState)
 *     IstmoRuntime.registerHandler(
 *         BiometricDispatcher.PLUGIN_ID,
 *         BiometricDispatcher(BiometricBackendImpl(this), BiometricCodecsImpl()),
 *     )
 * }
 * ```
 *
 * Cancelling the Rust call (dropping its future) cancels the coroutine,
 * which dismisses the prompt through `BiometricPrompt.cancelAuthentication`.
 *
 * Secrets are sealed with an AES-256-GCM Android Keystore key per alias
 * that only works inside a `BiometricPrompt.CryptoObject` — the key never
 * leaves secure hardware and cannot be used without a verified user. The
 * ciphertext lives in private `SharedPreferences`. Before API 30 the
 * Keystore cannot bind a per-use key to the device credential, so
 * `BiometricOrDeviceCredential` secrets fall back to a key usable for
 * [credentialValiditySeconds] after any unlock — keep it short.
 *
 * `enrollment_state` is the random generation of a sentinel Keystore key
 * that the OS invalidates whenever a biometric is enrolled or removed; a
 * new generation is minted the first time the invalidation is observed.
 */
class BiometricBackendImpl(
    private val activity: FragmentActivity,
    private val credentialValiditySeconds: Int = DEFAULT_AUTH_VALIDITY_SECONDS,
) : BiometricBackend {

    init {
        require(credentialValiditySeconds > 0) { "credentialValiditySeconds must be positive" }
    }

    private val manager: BiometricManager = BiometricManager.from(activity)
    private val vault: SharedPreferences =
        activity.getSharedPreferences(VAULT_PREFERENCES, Context.MODE_PRIVATE)
    private val enrollment: SharedPreferences =
        activity.getSharedPreferences(ENROLLMENT_PREFERENCES, Context.MODE_PRIVATE)
    private val keyStore: KeyStore by lazy {
        KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
    }

    /** `BiometricPrompt` only tolerates one prompt at a time. */
    private val promptMutex = Mutex()

    // ---------------------------------------------------------- Biometric impl

    override suspend fun availability(policy: AuthPolicy): Availability {
        val status = when (manager.canAuthenticate(authenticators(policy))) {
            BiometricManager.BIOMETRIC_SUCCESS -> BiometricStatus.Available
            BiometricManager.BIOMETRIC_ERROR_NONE_ENROLLED -> BiometricStatus.NoneEnrolled
            BiometricManager.BIOMETRIC_ERROR_NO_HARDWARE -> BiometricStatus.NoHardware
            BiometricManager.BIOMETRIC_ERROR_HW_UNAVAILABLE -> BiometricStatus.HardwareUnavailable
            BiometricManager.BIOMETRIC_ERROR_SECURITY_UPDATE_REQUIRED ->
                BiometricStatus.SecurityUpdateRequired
            BiometricManager.BIOMETRIC_ERROR_UNSUPPORTED -> BiometricStatus.Unsupported
            else -> BiometricStatus.HardwareUnavailable
        }
        val deviceSecure = keyguard().isDeviceSecure
        return Availability(
            status = status,
            kinds = sensorKinds(),
            deviceCredentialAvailable = deviceSecure,
            // Auth-bound Keystore keys need a secure lock screen.
            vaultAvailable = deviceSecure,
        )
    }

    override suspend fun authenticate(prompt: AuthPrompt): AuthMethod {
        checkPrompt(prompt)
        val result = showPrompt(promptInfo(prompt, authenticators(prompt.policy)), crypto = null)
        return authMethod(result.authenticationType)
    }

    override suspend fun store_secret(alias: String, secret: ByteArray, prompt: AuthPrompt) {
        checkAlias(alias)
        checkPrompt(prompt)
        val mode = KeyMode.of(prompt.policy)
        val key = withContext(Dispatchers.IO) { createKey(alias, mode) }
        val cipher = unlockedCipher(mode, prompt) { it.init(Cipher.ENCRYPT_MODE, key) }
        val sealed = crypt { cipher.doFinal(secret) }
        vault.edit().putString(alias, StoredSecret(mode, cipher.iv, sealed).encode()).apply()
    }

    override suspend fun read_secret(alias: String, prompt: AuthPrompt): ByteArray {
        checkAlias(alias)
        checkPrompt(prompt)
        val stored = vault.getString(alias, null)?.let(StoredSecret::decode)
            ?: throw BackendException(BiometricError.SecretNotFound)
        val key = withContext(Dispatchers.IO) { keyStore.getKey(keyAlias(alias), null) as? SecretKey }
            ?: invalidated(alias)
        val cipher = try {
            unlockedCipher(stored.mode, prompt) {
                it.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(GCM_TAG_BITS, stored.iv))
            }
        } catch (e: KeyPermanentlyInvalidatedException) {
            invalidated(alias)
        }
        return crypt { cipher.doFinal(stored.ciphertext) }
    }

    override suspend fun delete_secret(alias: String) {
        checkAlias(alias)
        withContext(Dispatchers.IO) { deleteQuietly(alias) }
    }

    override suspend fun has_secret(alias: String): Boolean {
        checkAlias(alias)
        return withContext(Dispatchers.IO) {
            vault.contains(alias) && keyStore.containsAlias(keyAlias(alias))
        }
    }

    override suspend fun enrollment_state(): ByteArray? = withContext(Dispatchers.IO) {
        // The sentinel needs an enrolled strong biometric to exist.
        if (manager.canAuthenticate(BIOMETRIC_STRONG) != BiometricManager.BIOMETRIC_SUCCESS) {
            return@withContext null
        }
        val generation = enrollment.getString(ENROLLMENT_GENERATION, null)
        if (generation != null && sentinelValid()) {
            return@withContext Base64.decode(generation, Base64.NO_WRAP)
        }
        createSentinel()
        val fresh = ByteArray(ENROLLMENT_GENERATION_BYTES).also { SecureRandom().nextBytes(it) }
        enrollment.edit()
            .putString(ENROLLMENT_GENERATION, Base64.encodeToString(fresh, Base64.NO_WRAP))
            .apply()
        fresh
    }

    // ------------------------------------------------------------- prompting

    private suspend fun showPrompt(
        info: BiometricPrompt.PromptInfo,
        crypto: BiometricPrompt.CryptoObject?,
    ): BiometricPrompt.AuthenticationResult = promptMutex.withLock {
        withContext(Dispatchers.Main) {
            suspendCancellableCoroutine { cont ->
                val callback = object : BiometricPrompt.AuthenticationCallback() {
                    override fun onAuthenticationSucceeded(
                        result: BiometricPrompt.AuthenticationResult,
                    ) {
                        if (cont.isActive) cont.resume(result)
                    }

                    override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                        if (cont.isActive) {
                            cont.resumeWithException(BackendException(error(errorCode, errString)))
                        }
                    }

                    // A single rejected attempt: the prompt stays up and
                    // lets the user retry, so there is nothing to resolve.
                    override fun onAuthenticationFailed() = Unit
                }
                val biometricPrompt = BiometricPrompt(
                    activity,
                    ContextCompat.getMainExecutor(activity),
                    callback,
                )
                cont.invokeOnCancellation {
                    activity.runOnUiThread { biometricPrompt.cancelAuthentication() }
                }
                if (crypto == null) {
                    biometricPrompt.authenticate(info)
                } else {
                    biometricPrompt.authenticate(info, crypto)
                }
            }
        }
    }

    private fun promptInfo(prompt: AuthPrompt, allowed: Int): BiometricPrompt.PromptInfo {
        val builder = BiometricPrompt.PromptInfo.Builder()
            .setTitle(prompt.title)
            .setDescription(prompt.reason)
            .setAllowedAuthenticators(allowed)
            .setConfirmationRequired(prompt.confirmationRequired)
        prompt.subtitle?.let(builder::setSubtitle)
        // The negative button is mandatory without DEVICE_CREDENTIAL and
        // forbidden with it.
        if (allowed and DEVICE_CREDENTIAL == 0) {
            builder.setNegativeButtonText(
                prompt.cancelLabel ?: activity.getString(android.R.string.cancel),
            )
        }
        return builder.build()
    }

    /**
     * `BIOMETRIC_STRONG | DEVICE_CREDENTIAL` is rejected on API 28-29;
     * the weak class is the strongest combination those releases accept.
     */
    private fun authenticators(policy: AuthPolicy): Int = when (policy) {
        AuthPolicy.BiometricStrong -> BIOMETRIC_STRONG
        AuthPolicy.BiometricWeak -> BIOMETRIC_WEAK
        AuthPolicy.BiometricOrDeviceCredential ->
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                BIOMETRIC_STRONG or DEVICE_CREDENTIAL
            } else {
                BIOMETRIC_WEAK or DEVICE_CREDENTIAL
            }
    }

    private fun checkPrompt(prompt: AuthPrompt) {
        if (prompt.title.isBlank()) {
            throw BackendException(BiometricError.InvalidPrompt("`title` must not be empty"))
        }
        if (prompt.reason.isBlank()) {
            throw BackendException(BiometricError.InvalidPrompt("`reason` must not be empty"))
        }
    }

    // ----------------------------------------------------------------- vault

    /** How a secret's Keystore key is unlocked. */
    private enum class KeyMode {
        /** Every use goes through a `CryptoObject`; strong biometrics only. */
        BiometricPerUse,

        /** Every use goes through a `CryptoObject`; biometrics or credential (API 30+). */
        CredentialPerUse,

        /** Usable for `credentialValiditySeconds` after any unlock (API < 30). */
        CredentialTimeBound;

        companion object {
            fun of(policy: AuthPolicy): KeyMode = when {
                policy != AuthPolicy.BiometricOrDeviceCredential -> BiometricPerUse
                Build.VERSION.SDK_INT >= Build.VERSION_CODES.R -> CredentialPerUse
                else -> CredentialTimeBound
            }
        }
    }

    /** `v1:<mode>:<iv>:<ciphertext>` with Base64 fields. */
    private class StoredSecret(val mode: KeyMode, val iv: ByteArray, val ciphertext: ByteArray) {
        fun encode(): String = listOf(
            FORMAT_VERSION,
            mode.name,
            Base64.encodeToString(iv, Base64.NO_WRAP),
            Base64.encodeToString(ciphertext, Base64.NO_WRAP),
        ).joinToString(":")

        companion object {
            fun decode(raw: String): StoredSecret {
                val parts = raw.split(":")
                if (parts.size != 4 || parts[0] != FORMAT_VERSION) {
                    throw BackendException(BiometricError.Backend("unrecognised vault entry"))
                }
                return StoredSecret(
                    KeyMode.valueOf(parts[1]),
                    Base64.decode(parts[2], Base64.NO_WRAP),
                    Base64.decode(parts[3], Base64.NO_WRAP),
                )
            }
        }
    }

    /**
     * Run [init] on a fresh cipher and get the user verified, in the order
     * the key's [mode] demands: per-use keys are initialised first and
     * unlocked through a `CryptoObject`; time-bound keys only initialise
     * after the unlock.
     */
    private suspend fun unlockedCipher(
        mode: KeyMode,
        prompt: AuthPrompt,
        init: (Cipher) -> Unit,
    ): Cipher {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        return when (mode) {
            KeyMode.BiometricPerUse, KeyMode.CredentialPerUse -> {
                init(cipher)
                val allowed = if (mode == KeyMode.BiometricPerUse) {
                    BIOMETRIC_STRONG
                } else {
                    BIOMETRIC_STRONG or DEVICE_CREDENTIAL
                }
                val result = showPrompt(
                    promptInfo(prompt, allowed),
                    BiometricPrompt.CryptoObject(cipher),
                )
                result.cryptoObject?.cipher ?: cipher
            }
            KeyMode.CredentialTimeBound -> {
                showPrompt(promptInfo(prompt, authenticators(prompt.policy)), crypto = null)
                try {
                    init(cipher)
                } catch (e: UserNotAuthenticatedException) {
                    // A weak biometric satisfied the prompt but does not
                    // unlock Keystore keys.
                    throw BackendException(BiometricError.AuthFailed)
                }
                cipher
            }
        }
    }

    /**
     * Whether the sentinel key still exists and survived every enrollment
     * change: initialising a cipher needs no user, but fails with
     * `KeyPermanentlyInvalidatedException` once the OS invalidated it.
     */
    private fun sentinelValid(): Boolean {
        val key = keyStore.getKey(ENROLLMENT_KEY_ALIAS, null) as? SecretKey ?: return false
        return try {
            Cipher.getInstance(TRANSFORMATION).init(Cipher.ENCRYPT_MODE, key)
            true
        } catch (e: KeyPermanentlyInvalidatedException) {
            false
        }
    }

    private fun createSentinel() {
        if (keyStore.containsAlias(ENROLLMENT_KEY_ALIAS)) keyStore.deleteEntry(ENROLLMENT_KEY_ALIAS)
        val spec = KeyGenParameterSpec.Builder(
            ENROLLMENT_KEY_ALIAS,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .setUserAuthenticationRequired(true)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
            spec.setInvalidatedByBiometricEnrollment(true)
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            spec.setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG)
        }
        KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
            .apply { init(spec.build()) }
            .generateKey()
    }

    private fun createKey(alias: String, mode: KeyMode): SecretKey {
        deleteQuietly(alias)
        val spec = KeyGenParameterSpec.Builder(
            keyAlias(alias),
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .setUserAuthenticationRequired(true)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
            spec.setInvalidatedByBiometricEnrollment(mode == KeyMode.BiometricPerUse)
        }
        when (mode) {
            KeyMode.BiometricPerUse ->
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                    spec.setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG)
                }
            KeyMode.CredentialPerUse ->
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                    spec.setUserAuthenticationParameters(
                        0,
                        KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL,
                    )
                }
            KeyMode.CredentialTimeBound ->
                @Suppress("DEPRECATION")
                spec.setUserAuthenticationValidityDurationSeconds(credentialValiditySeconds)
        }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE)
            .apply { init(spec.build()) }
            .generateKey()
    }

    /** The key is gone for good: drop the orphaned ciphertext too. */
    private fun invalidated(alias: String): Nothing {
        deleteQuietly(alias)
        throw BackendException(BiometricError.KeyInvalidated)
    }

    private fun deleteQuietly(alias: String) {
        vault.edit().remove(alias).apply()
        if (keyStore.containsAlias(keyAlias(alias))) keyStore.deleteEntry(keyAlias(alias))
    }

    private inline fun <T> crypt(block: () -> T): T = try {
        block()
    } catch (e: GeneralSecurityException) {
        throw BackendException(BiometricError.Backend("Keystore: ${e.message}"))
    }

    private fun checkAlias(alias: String) {
        if (!ALIAS_PATTERN.matches(alias)) {
            throw BackendException(BiometricError.InvalidAlias(alias))
        }
    }

    private fun keyAlias(alias: String) = "$KEY_ALIAS_PREFIX$alias"

    // --------------------------------------------------------------- helpers

    private fun sensorKinds(): List<BiometricKind> {
        val pm = activity.packageManager
        val kinds = buildList {
            if (pm.hasSystemFeature(PackageManager.FEATURE_FINGERPRINT)) add(BiometricKind.Fingerprint)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                if (pm.hasSystemFeature(PackageManager.FEATURE_FACE)) add(BiometricKind.Face)
                if (pm.hasSystemFeature(PackageManager.FEATURE_IRIS)) add(BiometricKind.Iris)
            }
        }
        val anyBiometric =
            manager.canAuthenticate(BIOMETRIC_WEAK) != BiometricManager.BIOMETRIC_ERROR_NO_HARDWARE
        return if (kinds.isEmpty() && anyBiometric) listOf(BiometricKind.Other) else kinds
    }

    private fun keyguard(): KeyguardManager =
        activity.getSystemService(Context.KEYGUARD_SERVICE) as KeyguardManager

    private fun authMethod(type: Int): AuthMethod = when (type) {
        BiometricPrompt.AUTHENTICATION_RESULT_TYPE_BIOMETRIC -> AuthMethod.Biometric
        BiometricPrompt.AUTHENTICATION_RESULT_TYPE_DEVICE_CREDENTIAL -> AuthMethod.DeviceCredential
        else -> AuthMethod.Unspecified
    }

    private fun error(code: Int, message: CharSequence): BiometricError = when (code) {
        BiometricPrompt.ERROR_USER_CANCELED,
        BiometricPrompt.ERROR_NEGATIVE_BUTTON -> BiometricError.UserCancelled
        BiometricPrompt.ERROR_CANCELED,
        BiometricPrompt.ERROR_TIMEOUT -> BiometricError.SystemCancelled
        BiometricPrompt.ERROR_LOCKOUT -> BiometricError.LockedOut
        BiometricPrompt.ERROR_LOCKOUT_PERMANENT -> BiometricError.LockedOutPermanent
        BiometricPrompt.ERROR_NO_BIOMETRICS,
        BiometricPrompt.ERROR_NO_DEVICE_CREDENTIAL ->
            BiometricError.NotAvailable(BiometricStatus.NoneEnrolled)
        BiometricPrompt.ERROR_HW_NOT_PRESENT ->
            BiometricError.NotAvailable(BiometricStatus.NoHardware)
        BiometricPrompt.ERROR_HW_UNAVAILABLE ->
            BiometricError.NotAvailable(BiometricStatus.HardwareUnavailable)
        BiometricPrompt.ERROR_SECURITY_UPDATE_REQUIRED ->
            BiometricError.NotAvailable(BiometricStatus.SecurityUpdateRequired)
        else -> BiometricError.Backend("BiometricPrompt error $code: $message")
    }

    companion object {
        /** Default of the `credentialValiditySeconds` constructor argument. */
        const val DEFAULT_AUTH_VALIDITY_SECONDS = 10

        private const val ANDROID_KEYSTORE = "AndroidKeyStore"
        private const val VAULT_PREFERENCES = "dev.istmo.biometric.vault"
        private const val KEY_ALIAS_PREFIX = "dev.istmo.biometric."
        private const val TRANSFORMATION = "AES/GCM/NoPadding"
        private const val GCM_TAG_BITS = 128
        private const val FORMAT_VERSION = "v1"
        private const val ENROLLMENT_PREFERENCES = "dev.istmo.biometric.enrollment"
        private const val ENROLLMENT_GENERATION = "generation"
        private const val ENROLLMENT_GENERATION_BYTES = 16
        private const val ENROLLMENT_KEY_ALIAS = "dev.istmo.biometric-enrollment-sentinel"
        private val ALIAS_PATTERN = Regex("^[A-Za-z0-9._-]{1,64}$")
    }
}
