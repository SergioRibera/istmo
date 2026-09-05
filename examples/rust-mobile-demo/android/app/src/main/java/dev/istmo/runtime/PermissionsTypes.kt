package dev.istmo.runtime

/**
 * Kotlin mirrors of the Rust `#[message]` types in
 * `istmo-plugins/src/permissions.rs`.
 *
 * `PermissionStatus` variant order must match the Rust declaration —
 * bincode encodes enum discriminants as u32 varints in declaration
 * order.
 */
enum class PermissionStatus {
    Granted,
    Denied,
    PermanentlyDenied,
    NotDetermined,
    NotSupported,
}

data class PermissionOutcome(
    val permission: String,
    val status: PermissionStatus,
)
