package dev.istmo.runtime

import java.io.ByteArrayOutputStream

/**
 * Wire codecs for the `Notifications` plugin.
 *
 * Encoding rules mirror `crates/plugins/src/notifications.rs`:
 *
 * * `NotificationRequest` — String title, String body, String channel_id,
 *   `NotificationImportance` discriminant, `Option<u32>` delay,
 *   `Option<String>` tag.
 * * `NotificationHandle` — single u32 varint (bincode encodes a
 *   single-field tuple struct as its inner field).
 * * `NotificationError` — u32 varint discriminant + per-variant payload.
 */
class NotificationsCodecsImpl : NotificationsCodecs {

    override fun readNotificationRequest(bytes: ByteArray, offset: Int): Bincode.Decoded<NotificationRequest> {
        var cursor = offset
        val title = Bincode.readString(bytes, cursor); cursor = title.consumed
        val body = Bincode.readString(bytes, cursor); cursor = body.consumed
        val channel = Bincode.readString(bytes, cursor); cursor = channel.consumed
        val importance = readNotificationImportance(bytes, cursor); cursor = importance.consumed
        val delay = Bincode.readOption(bytes, cursor) { p, o ->
            val (v, next) = Bincode.readVarintU64(p, o)
            Bincode.Decoded(v.toUInt(), next)
        }
        cursor = delay.consumed
        val tag = Bincode.readOption(bytes, cursor, Bincode::readString); cursor = tag.consumed
        return Bincode.Decoded(
            NotificationRequest(
                title = title.value,
                body = body.value,
                channelId = channel.value,
                importance = importance.value,
                delaySeconds = delay.value,
                tag = tag.value,
            ),
            cursor,
        )
    }

    override fun writeNotificationRequest(out: ByteArrayOutputStream, value: NotificationRequest) {
        Bincode.writeString(out, value.title)
        Bincode.writeString(out, value.body)
        Bincode.writeString(out, value.channelId)
        writeNotificationImportance(out, value.importance)
        Bincode.writeOption(out, value.delaySeconds) { s, v ->
            Bincode.writeVarintU64(s, v.toLong())
        }
        Bincode.writeOption(out, value.tag) { s, v -> Bincode.writeString(s, v) }
    }

    override fun readNotificationHandle(bytes: ByteArray, offset: Int): Bincode.Decoded<NotificationHandle> {
        val (v, next) = Bincode.readVarintU64(bytes, offset)
        return Bincode.Decoded(NotificationHandle(v.toUInt()), next)
    }

    override fun writeNotificationHandle(out: ByteArrayOutputStream, value: NotificationHandle) {
        Bincode.writeVarintU64(out, value.id.toLong())
    }

    override fun readNotificationError(bytes: ByteArray, offset: Int): Bincode.Decoded<NotificationError> {
        var cursor = offset
        val (disc, next) = Bincode.readVarintU64(bytes, cursor); cursor = next
        return when (disc.toInt()) {
            0 -> Bincode.Decoded(NotificationError.PermissionDenied, cursor)
            1 -> {
                val msg = Bincode.readString(bytes, cursor)
                Bincode.Decoded(NotificationError.InvalidChannel(msg.value), msg.consumed)
            }
            2 -> {
                val msg = Bincode.readString(bytes, cursor)
                Bincode.Decoded(NotificationError.Scheduler(msg.value), msg.consumed)
            }
            else -> error("NotificationError: unknown discriminant $disc")
        }
    }

    override fun writeNotificationError(out: ByteArrayOutputStream, value: NotificationError) {
        when (value) {
            NotificationError.PermissionDenied -> Bincode.writeEnumDiscriminant(out, 0)
            is NotificationError.InvalidChannel -> {
                Bincode.writeEnumDiscriminant(out, 1)
                Bincode.writeString(out, value.message)
            }
            is NotificationError.Scheduler -> {
                Bincode.writeEnumDiscriminant(out, 2)
                Bincode.writeString(out, value.message)
            }
        }
    }

    private fun readNotificationImportance(bytes: ByteArray, offset: Int): Bincode.Decoded<NotificationImportance> {
        val (disc, next) = Bincode.readVarintU64(bytes, offset)
        val variant = NotificationImportance.entries.getOrNull(disc.toInt())
            ?: error("NotificationImportance: unknown discriminant $disc")
        return Bincode.Decoded(variant, next)
    }

    private fun writeNotificationImportance(out: ByteArrayOutputStream, value: NotificationImportance) {
        Bincode.writeEnumDiscriminant(out, value.ordinal)
    }
}
