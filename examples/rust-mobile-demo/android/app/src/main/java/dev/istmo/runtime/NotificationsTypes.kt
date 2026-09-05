package dev.istmo.runtime

/**
 * Kotlin mirrors of the Rust `#[message]` types in
 * `istmo-plugins/src/notifications.rs`.
 */
enum class NotificationImportance {
    Min,
    Low,
    Default,
    High,
}

data class NotificationRequest(
    val title: String,
    val body: String,
    val channelId: String,
    val importance: NotificationImportance,
    val delaySeconds: UInt?,
    val tag: String?,
)

data class NotificationHandle(val id: UInt)

sealed class NotificationError {
    object PermissionDenied : NotificationError()
    data class InvalidChannel(val message: String) : NotificationError()
    data class Scheduler(val message: String) : NotificationError()
}
