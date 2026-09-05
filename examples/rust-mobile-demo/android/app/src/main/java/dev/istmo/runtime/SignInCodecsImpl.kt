package dev.istmo.runtime

import java.io.ByteArrayOutputStream

/**
 * Wire codecs for the `SignIn` plugin.
 *
 * Wire schema mirrors `crates/plugins/src/google_sign_in.rs`:
 *
 * * `SignInMode` — u32 varint discriminant.
 * * `SignInConfig` — String, Vec<String>, Option<String>, Option<String>, Bool.
 * * `SignInAccount` — String, Option<String>×3, String, Vec<String>, u64 varint.
 * * `SignInError` — u32 varint discriminant + Option<String> payload for
 *   InvalidConfiguration / Network / Backend.
 * * `NativeHandleId` — u64 varint.
 */
class SignInCodecsImpl : SignInCodecs {

    override fun readSignInMode(bytes: ByteArray, offset: Int): Bincode.Decoded<SignInMode> {
        val (disc, next) = Bincode.readVarintU64(bytes, offset)
        val variant = SignInMode.entries.getOrNull(disc.toInt())
            ?: error("SignInMode: unknown discriminant $disc")
        return Bincode.Decoded(variant, next)
    }

    override fun writeSignInMode(out: ByteArrayOutputStream, value: SignInMode) {
        Bincode.writeEnumDiscriminant(out, value.ordinal)
    }

    override fun readSignInConfig(bytes: ByteArray, offset: Int): Bincode.Decoded<SignInConfig> {
        var cursor = offset
        val server = Bincode.readString(bytes, cursor); cursor = server.consumed
        val scopes = Bincode.readVec(bytes, cursor, Bincode::readString); cursor = scopes.consumed
        val hosted = Bincode.readOption(bytes, cursor, Bincode::readString); cursor = hosted.consumed
        val nonce = Bincode.readOption(bytes, cursor, Bincode::readString); cursor = nonce.consumed
        val auto = Bincode.readBool(bytes, cursor); cursor = auto.consumed
        return Bincode.Decoded(
            SignInConfig(server.value, scopes.value, hosted.value, nonce.value, auto.value),
            cursor,
        )
    }

    override fun writeSignInConfig(out: ByteArrayOutputStream, value: SignInConfig) {
        Bincode.writeString(out, value.serverClientId)
        Bincode.writeVec(out, value.scopes, Bincode::writeString)
        Bincode.writeOption(out, value.hostedDomain, Bincode::writeString)
        Bincode.writeOption(out, value.nonce, Bincode::writeString)
        Bincode.writeBool(out, value.autoSelect)
    }

    override fun readSignInAccount(bytes: ByteArray, offset: Int): Bincode.Decoded<SignInAccount> {
        var cursor = offset
        val id = Bincode.readString(bytes, cursor); cursor = id.consumed
        val email = Bincode.readOption(bytes, cursor, Bincode::readString); cursor = email.consumed
        val display = Bincode.readOption(bytes, cursor, Bincode::readString); cursor = display.consumed
        val photo = Bincode.readOption(bytes, cursor, Bincode::readString); cursor = photo.consumed
        val idToken = Bincode.readString(bytes, cursor); cursor = idToken.consumed
        val scopes = Bincode.readVec(bytes, cursor, Bincode::readString); cursor = scopes.consumed
        val credential = readNativeHandleId(bytes, cursor); cursor = credential.consumed
        return Bincode.Decoded(
            SignInAccount(
                id = id.value,
                email = email.value,
                displayName = display.value,
                photoUrl = photo.value,
                idToken = idToken.value,
                grantedScopes = scopes.value,
                credential = credential.value,
            ),
            cursor,
        )
    }

    override fun writeSignInAccount(out: ByteArrayOutputStream, value: SignInAccount) {
        Bincode.writeString(out, value.id)
        Bincode.writeOption(out, value.email, Bincode::writeString)
        Bincode.writeOption(out, value.displayName, Bincode::writeString)
        Bincode.writeOption(out, value.photoUrl, Bincode::writeString)
        Bincode.writeString(out, value.idToken)
        Bincode.writeVec(out, value.grantedScopes, Bincode::writeString)
        writeNativeHandleId(out, value.credential)
    }

    override fun readSignInError(bytes: ByteArray, offset: Int): Bincode.Decoded<SignInError> {
        var cursor = offset
        val (disc, next) = Bincode.readVarintU64(bytes, cursor); cursor = next
        return when (disc.toInt()) {
            0 -> Bincode.Decoded(SignInError.UserCancelled, cursor)
            1 -> Bincode.Decoded(SignInError.NoCredentialAvailable, cursor)
            2 -> Bincode.Decoded(SignInError.Reauthenticate, cursor)
            3 -> {
                val msg = Bincode.readString(bytes, cursor)
                Bincode.Decoded(SignInError.InvalidConfiguration(msg.value), msg.consumed)
            }
            4 -> {
                val msg = Bincode.readString(bytes, cursor)
                Bincode.Decoded(SignInError.Network(msg.value), msg.consumed)
            }
            5 -> {
                val msg = Bincode.readString(bytes, cursor)
                Bincode.Decoded(SignInError.Backend(msg.value), msg.consumed)
            }
            else -> error("SignInError: unknown discriminant $disc")
        }
    }

    override fun writeSignInError(out: ByteArrayOutputStream, value: SignInError) {
        when (value) {
            SignInError.UserCancelled -> Bincode.writeEnumDiscriminant(out, 0)
            SignInError.NoCredentialAvailable -> Bincode.writeEnumDiscriminant(out, 1)
            SignInError.Reauthenticate -> Bincode.writeEnumDiscriminant(out, 2)
            is SignInError.InvalidConfiguration -> {
                Bincode.writeEnumDiscriminant(out, 3)
                Bincode.writeString(out, value.message)
            }
            is SignInError.Network -> {
                Bincode.writeEnumDiscriminant(out, 4)
                Bincode.writeString(out, value.message)
            }
            is SignInError.Backend -> {
                Bincode.writeEnumDiscriminant(out, 5)
                Bincode.writeString(out, value.message)
            }
        }
    }

    override fun readNativeHandleId(bytes: ByteArray, offset: Int): Bincode.Decoded<NativeHandleId> {
        val (v, next) = Bincode.readVarintU64(bytes, offset)
        return Bincode.Decoded(v, next)
    }

    override fun writeNativeHandleId(out: ByteArrayOutputStream, value: NativeHandleId) {
        Bincode.writeVarintU64(out, value)
    }
}
