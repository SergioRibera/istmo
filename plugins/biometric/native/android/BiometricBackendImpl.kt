package dev.istmo.plugins.biometric

import android.app.KeyguardManager
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
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
 */
class BiometricBackendImpl(
    private val activity: FragmentActivity,
) : BiometricBackend {

    private val manager: BiometricManager = BiometricManager.from(activity)

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
        return Availability(
            status = status,
            kinds = sensorKinds(),
            deviceCredentialAvailable = keyguard().isDeviceSecure,
        )
    }

    override suspend fun authenticate(prompt: AuthPrompt): AuthMethod {
        if (prompt.title.isBlank()) {
            throw BackendException(BiometricError.InvalidPrompt("`title` must not be empty"))
        }
        if (prompt.reason.isBlank()) {
            throw BackendException(BiometricError.InvalidPrompt("`reason` must not be empty"))
        }
        val info = promptInfo(prompt)
        return promptMutex.withLock {
            withContext(Dispatchers.Main) { showPrompt(info) }
        }
    }

    // ---------------------------------------------------------------- helpers

    private suspend fun showPrompt(info: BiometricPrompt.PromptInfo): AuthMethod =
        suspendCancellableCoroutine { cont ->
            val callback = object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                    if (cont.isActive) cont.resume(authMethod(result.authenticationType))
                }

                override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                    if (cont.isActive) {
                        cont.resumeWithException(BackendException(error(errorCode, errString)))
                    }
                }

                // A single rejected attempt: the prompt stays up and lets
                // the user retry, so there is nothing to resolve yet.
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
            biometricPrompt.authenticate(info)
        }

    private fun promptInfo(prompt: AuthPrompt): BiometricPrompt.PromptInfo {
        val allowed = authenticators(prompt.policy)
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
}
