package dev.istmo.plugins.biometric

import android.app.Activity
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
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.IstmoActivityBound
import java.lang.ref.WeakReference
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
    activity: FragmentActivity,
    private val credentialValiditySeconds: Int = DEFAULT_AUTH_VALIDITY_SECONDS,
) : BiometricBackend, IstmoActivityBound {

    init {
        require(credentialValiditySeconds > 0) { "credentialValiditySeconds must be positive" }
    }

    // Weak, so a destroyed activity is never kept alive; the registry builds
    // the backend once and hands it each new activity ([attachActivity]),
    // keeping the vault lock and prompt state across recreation.
    @Volatile
    private var activityRef = WeakReference(activity)

    override fun attachActivity(activity: Activity) {
        // The registry only hands over activities of the declared class.
        activityRef = WeakReference(activity as FragmentActivity)
    }
    private val appContext: Context = activity.applicationContext
    private val manager: BiometricManager = BiometricManager.from(appContext)
    private val vault: SharedPreferences =
        appContext.getSharedPreferences(VAULT_PREFERENCES, Context.MODE_PRIVATE)
    private val enrollment: SharedPreferences =
        appContext.getSharedPreferences(ENROLLMENT_PREFERENCES, Context.MODE_PRIVATE)
    private val keyStore: KeyStore by lazy {
        KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
    }

    /** `BiometricPrompt` only tolerates one prompt at a time. */
    private val promptMutex = Mutex()

    /**
     * Serialises vault reads and writes: a read racing a store could load
     * an entry whose key the store then deletes, and wipe the new secret
     * as invalidated. Always taken before [promptMutex].
     */
    private val vaultMutex = Mutex()

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
        vaultMutex.withLock { storeLocked(alias, secret, prompt) }
    }

    private suspend fun storeLocked(alias: String, secret: ByteArray, prompt: AuthPrompt) {
        val mode = KeyMode.of(prompt.policy)
        // The new secret gets a key under a fresh alias; the previous key
        // and ciphertext stay usable until the new one is sealed, so a
        // cancelled or failed prompt leaves the stored secret intact. A
        // corrupt previous entry must not block overwriting it.
        val previous = runCatching { storedSecret(alias) }.getOrNull()
        val newKeyAlias = freshKeyAlias(alias)
        val sealed = try {
            val key = withContext(Dispatchers.IO) { createKey(newKeyAlias, mode) }
            val cipher = unlockedCipher(mode, prompt) { it.init(Cipher.ENCRYPT_MODE, key) }
            StoredSecret(mode, newKeyAlias, cipher.iv, crypt { cipher.doFinal(secret) })
        } catch (e: Throwable) {
            // Runs for cancellation too; a Keystore delete does not suspend.
            deleteKey(newKeyAlias)
            throw e
        }
        withContext(Dispatchers.IO) {
            // `commit` so the entry is durable before the old key is gone.
            if (!vault.edit().putString(alias, sealed.encode()).commit()) {
                // Nothing durable references the new key, and the previous
                // entry and its key are untouched.
                deleteKey(newKeyAlias)
                throw BackendException(BiometricError.Backend("could not persist the vault entry"))
            }
            previous?.keyAlias?.takeIf { it != newKeyAlias }?.let(::deleteKey)
        }
    }

    override suspend fun read_secret(alias: String, prompt: AuthPrompt): ByteArray {
        checkAlias(alias)
        checkPrompt(prompt)
        return vaultMutex.withLock { readLocked(alias, prompt) }
    }

    private suspend fun readLocked(alias: String, prompt: AuthPrompt): ByteArray {
        val stored = storedSecret(alias) ?: throw BackendException(BiometricError.SecretNotFound)
        val key = withContext(Dispatchers.IO) { keyStore.getKey(stored.keyAlias, null) as? SecretKey }
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
        vaultMutex.withLock { withContext(Dispatchers.IO) { deleteQuietly(alias) } }
    }

    override suspend fun has_secret(alias: String): Boolean {
        checkAlias(alias)
        return withContext(Dispatchers.IO) {
            val stored = runCatching { storedSecret(alias) }.getOrNull()
            stored != null && keyStore.containsAlias(stored.keyAlias)
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
            val activity = activityRef.get()
                ?: throw BackendException(BiometricError.Backend("host activity is gone"))
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
                prompt.cancelLabel ?: appContext.getString(android.R.string.cancel),
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

    /** `v2:<mode>:<key alias>:<iv>:<ciphertext>` with Base64 binary fields. */
    private class StoredSecret(
        val mode: KeyMode,
        val keyAlias: String,
        val iv: ByteArray,
        val ciphertext: ByteArray,
    ) {
        fun encode(): String = listOf(
            FORMAT_VERSION,
            mode.name,
            keyAlias,
            Base64.encodeToString(iv, Base64.NO_WRAP),
            Base64.encodeToString(ciphertext, Base64.NO_WRAP),
        ).joinToString(":")

        companion object {
            fun decode(raw: String): StoredSecret {
                val parts = raw.split(":")
                return try {
                    if (parts.size != 5 || parts[0] != FORMAT_VERSION) {
                        throw BackendException(BiometricError.Backend("unrecognised vault entry"))
                    }
                    StoredSecret(
                        KeyMode.valueOf(parts[1]),
                        parts[2],
                        Base64.decode(parts[3], Base64.NO_WRAP),
                        Base64.decode(parts[4], Base64.NO_WRAP),
                    )
                } catch (e: IllegalArgumentException) {
                    // Unknown key mode or malformed Base64.
                    throw BackendException(BiometricError.Backend("corrupt vault entry: ${e.message}"))
                }
            }
        }
    }

    private fun storedSecret(alias: String): StoredSecret? =
        vault.getString(alias, null)?.let(StoredSecret::decode)

    /** A key alias no previous store of [alias] used. */
    private fun freshKeyAlias(alias: String): String {
        val suffix = ByteArray(KEY_ALIAS_SUFFIX_BYTES).also { SecureRandom().nextBytes(it) }
            .joinToString("") { "%02x".format(it) }
        return "${keyAlias(alias)}.$suffix"
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

    private fun createKey(keyAlias: String, mode: KeyMode): SecretKey {
        val spec = KeyGenParameterSpec.Builder(
            keyAlias,
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
        val stored = runCatching { storedSecret(alias) }.getOrNull()
        vault.edit().remove(alias).apply()
        stored?.keyAlias?.let(::deleteKey)
    }

    private fun deleteKey(keyAlias: String) {
        runCatching { if (keyStore.containsAlias(keyAlias)) keyStore.deleteEntry(keyAlias) }
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
        val pm = appContext.packageManager
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
        appContext.getSystemService(Context.KEYGUARD_SERVICE) as KeyguardManager

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
        private const val FORMAT_VERSION = "v2"
        private const val KEY_ALIAS_SUFFIX_BYTES = 8
        private const val ENROLLMENT_PREFERENCES = "dev.istmo.biometric.enrollment"
        private const val ENROLLMENT_GENERATION = "generation"
        private const val ENROLLMENT_GENERATION_BYTES = 16
        private const val ENROLLMENT_KEY_ALIAS = "dev.istmo.biometric-enrollment-sentinel"
        private val ALIAS_PATTERN = Regex("^[A-Za-z0-9._-]{1,64}$")
    }
}
