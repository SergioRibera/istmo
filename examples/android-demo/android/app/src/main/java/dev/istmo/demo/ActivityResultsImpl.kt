package dev.istmo.demo

import android.content.ActivityNotFoundException
import android.content.Intent
import android.net.Uri
import dev.istmo.runtime.Bincode
import dev.istmo.runtime.PluginException
import dev.istmo.runtime.PluginHandler
import java.io.ByteArrayOutputStream
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Kotlin backend for the `istmo.activity_results` plugin.
 *
 * The demo implements the ok-path for `VIEW`-style intents by handing the
 * intent to `startActivity` and immediately reporting `ActivityOutcome::Ok`.
 * A production backend would use `registerForActivityResult` to wait for the
 * real result; that's left as an exercise so the demo stays focused on the
 * wire round-trip.
 *
 * Wire types decoded / encoded here match the Rust `IntentRequest`,
 * `ActivityResult`, `ActivityOutcome`, `ExtraValue` and `ActivityLaunchError`.
 */
class ActivityResultsImpl : PluginHandler {

    private var host: ActivityResultsHost? = null

    fun attach(host: ActivityResultsHost) { this.host = host }
    fun detach() { this.host = null }

    override suspend fun handleCall(
        instanceId: Long,
        method: String,
        payload: ByteArray,
    ): ByteArray = when (method) {
        "launch" -> {
            val (request, _) = IntentRequestCodec.read(payload, 0)
            withContext(Dispatchers.Main) { launch(request) }
        }
        else -> error("unknown ActivityResults method: $method")
    }

    private fun launch(request: IntentRequestValue): ByteArray {
        val host = this.host
            ?: throw PluginException(encodeLaunchError(LaunchErrorValue.NotAuthorized))
        val intent = buildIntent(request)
        return try {
            host.activity.startActivity(intent)
            val result = ActivityResultValue(
                outcome = ActivityOutcomeValue.Ok,
                dataUri = request.uri,
                extras = emptyList(),
            )
            ActivityResultCodec.write(result)
        } catch (_: ActivityNotFoundException) {
            throw PluginException(encodeLaunchError(LaunchErrorValue.NoActivityFound))
        } catch (t: Throwable) {
            throw PluginException(
                encodeLaunchError(LaunchErrorValue.Platform(t.message ?: t::class.java.simpleName)),
            )
        }
    }

    private fun buildIntent(request: IntentRequestValue): Intent {
        val intent = Intent(request.action)
        request.uri?.let { intent.data = Uri.parse(it) }
        request.mimeType?.let { intent.type = it }
        request.component?.let { (pkg, cls) -> intent.setClassName(pkg, cls) }
        for (category in request.categories) intent.addCategory(category)
        for ((key, value) in request.extras) {
            when (value) {
                is ExtraValueValue.Text -> intent.putExtra(key, value.value)
                is ExtraValueValue.Int -> intent.putExtra(key, value.value)
                is ExtraValueValue.Bool -> intent.putExtra(key, value.value)
                is ExtraValueValue.Bytes -> intent.putExtra(key, value.value)
            }
        }
        return intent
    }

    private fun encodeLaunchError(err: LaunchErrorValue): ByteArray {
        val out = ByteArrayOutputStream()
        when (err) {
            is LaunchErrorValue.NoActivityFound -> Bincode.writeEnumDiscriminant(out, 0)
            is LaunchErrorValue.NotAuthorized -> Bincode.writeEnumDiscriminant(out, 1)
            is LaunchErrorValue.Platform -> {
                Bincode.writeEnumDiscriminant(out, 2)
                Bincode.writeString(out, err.message)
            }
        }
        return out.toByteArray()
    }
}

interface ActivityResultsHost {
    val activity: android.app.Activity
}

// ---- Value mirrors of the Rust plugin types ----

data class IntentRequestValue(
    val action: String,
    val uri: String?,
    val component: Pair<String, String>?,
    val mimeType: String?,
    val categories: List<String>,
    val extras: List<Pair<String, ExtraValueValue>>,
)

data class ActivityResultValue(
    val outcome: ActivityOutcomeValue,
    val dataUri: String?,
    val extras: List<Pair<String, ExtraValueValue>>,
)

sealed class ActivityOutcomeValue {
    object Ok : ActivityOutcomeValue()
    object Cancelled : ActivityOutcomeValue()
    data class Custom(val code: Int) : ActivityOutcomeValue()
}

sealed class ExtraValueValue {
    data class Text(val value: String) : ExtraValueValue()
    data class Int(val value: Long) : ExtraValueValue()
    data class Bool(val value: Boolean) : ExtraValueValue()
    data class Bytes(val value: ByteArray) : ExtraValueValue()
}

sealed class LaunchErrorValue {
    object NoActivityFound : LaunchErrorValue()
    object NotAuthorized : LaunchErrorValue()
    data class Platform(val message: String) : LaunchErrorValue()
}

// ---- Codecs matching the bincode wire format ----

private object IntentRequestCodec {
    fun read(payload: ByteArray, offset: Int): Bincode.Decoded<IntentRequestValue> {
        val action = Bincode.readString(payload, offset)
        val uri = Bincode.readOption(payload, action.consumed, Bincode::readString)
        val component = Bincode.readOption(payload, uri.consumed) { buf, off ->
            Bincode.readPair(buf, off, Bincode::readString, Bincode::readString)
        }
        val mime = Bincode.readOption(payload, component.consumed, Bincode::readString)
        val categories = Bincode.readVec(payload, mime.consumed, Bincode::readString)
        val extras = Bincode.readVec(payload, categories.consumed) { buf, off ->
            Bincode.readPair(buf, off, Bincode::readString, ExtraValueCodec::read)
        }
        val value = IntentRequestValue(
            action = action.value,
            uri = uri.value,
            component = component.value,
            mimeType = mime.value,
            categories = categories.value,
            extras = extras.value,
        )
        return Bincode.Decoded(value, extras.consumed)
    }
}

private object ActivityResultCodec {
    fun write(value: ActivityResultValue): ByteArray {
        val out = ByteArrayOutputStream()
        when (val outcome = value.outcome) {
            is ActivityOutcomeValue.Ok -> Bincode.writeEnumDiscriminant(out, 0)
            is ActivityOutcomeValue.Cancelled -> Bincode.writeEnumDiscriminant(out, 1)
            is ActivityOutcomeValue.Custom -> {
                Bincode.writeEnumDiscriminant(out, 2)
                Bincode.writeVarintI64(out, outcome.code.toLong())
            }
        }
        Bincode.writeOption(out, value.dataUri, Bincode::writeString)
        Bincode.writeVec(out, value.extras) { sink, pair ->
            Bincode.writeString(sink, pair.first)
            ExtraValueCodec.write(sink, pair.second)
        }
        return out.toByteArray()
    }
}

private object ExtraValueCodec {
    fun read(payload: ByteArray, offset: Int): Bincode.Decoded<ExtraValueValue> {
        val (disc, next) = Bincode.readEnumDiscriminant(payload, offset)
        return when (disc) {
            0 -> {
                val s = Bincode.readString(payload, next)
                Bincode.Decoded(ExtraValueValue.Text(s.value), s.consumed)
            }
            1 -> {
                val (n, after) = Bincode.readVarintI64(payload, next)
                Bincode.Decoded(ExtraValueValue.Int(n), after)
            }
            2 -> {
                val b = Bincode.readBool(payload, next)
                Bincode.Decoded(ExtraValueValue.Bool(b.value), b.consumed)
            }
            3 -> {
                val (length, after) = Bincode.readVarintU64(payload, next)
                val end = after + length.toInt()
                val bytes = payload.copyOfRange(after, end)
                Bincode.Decoded(ExtraValueValue.Bytes(bytes), end)
            }
            else -> error("unknown ExtraValue discriminant: $disc")
        }
    }

    fun write(out: ByteArrayOutputStream, value: ExtraValueValue) {
        when (value) {
            is ExtraValueValue.Text -> {
                Bincode.writeEnumDiscriminant(out, 0)
                Bincode.writeString(out, value.value)
            }
            is ExtraValueValue.Int -> {
                Bincode.writeEnumDiscriminant(out, 1)
                Bincode.writeVarintI64(out, value.value)
            }
            is ExtraValueValue.Bool -> {
                Bincode.writeEnumDiscriminant(out, 2)
                Bincode.writeBool(out, value.value)
            }
            is ExtraValueValue.Bytes -> {
                Bincode.writeEnumDiscriminant(out, 3)
                Bincode.writeVarintU64(out, value.value.size.toLong())
                out.write(value.value)
            }
        }
    }
}
