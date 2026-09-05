package dev.istmo.runtime

import java.io.ByteArrayOutputStream

/**
 * Wire codecs for the `Permissions` plugin.
 *
 * `PermissionStatus` is a u32 varint discriminant. `PermissionOutcome`
 * is a String followed by that discriminant, no length prefix.
 */
class PermissionsCodecsImpl : PermissionsCodecs {

    override fun readPermissionStatus(bytes: ByteArray, offset: Int): Bincode.Decoded<PermissionStatus> {
        val (disc, next) = Bincode.readVarintU64(bytes, offset)
        val variant = PermissionStatus.entries.getOrNull(disc.toInt())
            ?: error("PermissionStatus: unknown discriminant $disc")
        return Bincode.Decoded(variant, next)
    }

    override fun writePermissionStatus(out: ByteArrayOutputStream, value: PermissionStatus) {
        Bincode.writeEnumDiscriminant(out, value.ordinal)
    }

    override fun readPermissionOutcome(bytes: ByteArray, offset: Int): Bincode.Decoded<PermissionOutcome> {
        val permission = Bincode.readString(bytes, offset)
        val status = readPermissionStatus(bytes, permission.consumed)
        return Bincode.Decoded(PermissionOutcome(permission.value, status.value), status.consumed)
    }

    override fun writePermissionOutcome(out: ByteArrayOutputStream, value: PermissionOutcome) {
        Bincode.writeString(out, value.permission)
        writePermissionStatus(out, value.status)
    }
}
