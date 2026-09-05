package dev.istmo.runtime

/**
 * Kotlin mirrors of the Rust `#[message]` types in
 * `istmo-plugins/src/google_sign_in.rs`.
 */
enum class SignInMode {
    Interactive,
    SilentOnly,
}

data class SignInConfig(
    val serverClientId: String,
    val scopes: List<String>,
    val hostedDomain: String?,
    val nonce: String?,
    val autoSelect: Boolean,
)

data class SignInAccount(
    val id: String,
    val email: String?,
    val displayName: String?,
    val photoUrl: String?,
    val idToken: String,
    val grantedScopes: List<String>,
    val credential: NativeHandleId,
)

sealed class SignInError {
    object UserCancelled : SignInError()
    object NoCredentialAvailable : SignInError()
    object Reauthenticate : SignInError()
    data class InvalidConfiguration(val message: String) : SignInError()
    data class Network(val message: String) : SignInError()
    data class Backend(val message: String) : SignInError()
}
