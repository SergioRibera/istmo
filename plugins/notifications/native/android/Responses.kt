package dev.istmo.plugins.notifications

import android.content.Context
import android.util.Log
import dev.istmo.runtime.Bincode
import dev.istmo.runtime.IstmoBytesOut
import dev.istmo.runtime.IstmoRuntime
import java.io.ByteArrayOutputStream
import java.io.File

/**
 * The responses lane (`istmo.notifications.responses`, an early queue of
 * capacity 64) and the deep links of responses with a URL.
 *
 * Foreground responses (the trampoline activity) always go through
 * [IstmoRuntime.publishEarlyQueue], which holds them in memory until the
 * runtime starts in this process (the trampoline starts the app right
 * after). Background responses received while no runtime runs in the
 * process (a receiver woke a cold process) are appended to a file under
 * `noBackupFilesDir` and drained into the lane when the backend is
 * registered, so they survive the process dying first.
 */
internal object Responses {
    private const val TAG = "istmo.notifications"

    /** `istmo_notifications::RESPONSES_CHANNEL` / `RESPONSES_CAPACITY`. */
    const val CHANNEL = "istmo.notifications.responses"
    const val CAPACITY = 64

    /** `istmo_plugins::DEEPLINKS_CHANNEL` / `DEFAULT_DEEPLINKS_CAPACITY`. */
    private const val DEEPLINKS_CHANNEL = "istmo.deeplinks"
    private const val DEEPLINKS_CAPACITY = 16

    /** Format byte of the queue file; then wire `Vec<Vec<u8>>` of encoded responses. */
    private const val QUEUE_FORMAT = 1
    private const val MAX_QUEUED = 64

    val codecs = NotificationsCodecsImpl()

    fun encode(response: NotificationResponse): ByteArray {
        val out = IstmoBytesOut(128)
        codecs.writeNotificationResponse(out, response)
        return out.toExactArray()
    }

    fun decode(bytes: ByteArray): NotificationResponse = codecs.readNotificationResponse(bytes, 0).value

    /** Publishes [response] on the lane now (and its deep link). */
    fun publish(response: NotificationResponse) {
        IstmoRuntime.publishEarlyQueue(CHANNEL, CAPACITY, encode(response))
        deepLink(response)?.let { IstmoRuntime.publishEarlyQueue(DEEPLINKS_CHANNEL, DEEPLINKS_CAPACITY, it) }
    }

    /**
     * `DeepLink { uri, source, received_at_ms }` of [response]: only taps
     * and buttons with a URL publish one (`Dismiss` and `More` never do).
     * Source `notification:<id>` or `notification:<id>:<action>`.
     */
    fun deepLink(response: NotificationResponse): ByteArray? {
        val url = response.url ?: return null
        val source = when (val action = response.action) {
            is ResponseAction.Open -> "notification:${response.id.value}"
            is ResponseAction.Button -> "notification:${response.id.value}:${action.value.value}"
            else -> return null
        }
        val out = ByteArrayOutputStream()
        Bincode.writeString(out, url)
        Bincode.writeOption(out, source) { sink, s -> Bincode.writeString(sink, s) }
        Bincode.writeOption(out, System.currentTimeMillis()) { sink, ms -> Bincode.writeVarintU64(sink, ms) }
        return out.toByteArray()
    }

    private fun queueFile(context: Context) = File(File(context.noBackupFilesDir, "istmo-notifications"), "responses.bin")

    /** Appends [response] to the file queue (oldest dropped past 64). */
    fun enqueue(context: Context, response: NotificationResponse) = synchronized(this) {
        val file = queueFile(context)
        val items = ArrayList(read(file))
        items.add(encode(response))
        while (items.size > MAX_QUEUED) items.removeAt(0)
        write(file, items)
    }

    /** Publishes and removes everything [enqueue] stored. */
    fun drain(context: Context) {
        val items = synchronized(this) {
            val file = queueFile(context)
            val items = read(file)
            file.delete()
            items
        }
        for (bytes in items) {
            try {
                publish(decode(bytes))
            } catch (e: IllegalArgumentException) {
                Log.w(TAG, "dropping an unreadable queued response", e)
            }
        }
    }

    private fun read(file: File): List<ByteArray> {
        val bytes = try {
            file.readBytes()
        } catch (_: java.io.IOException) {
            return emptyList()
        }
        if (bytes.isEmpty() || bytes[0].toInt() != QUEUE_FORMAT) return emptyList()
        return try {
            Bincode.readVec(bytes, 1) { b, o -> Bincode.readBytes(b, o) }.value
        } catch (_: IllegalArgumentException) {
            emptyList()
        }
    }

    private fun write(file: File, items: List<ByteArray>) {
        file.parentFile?.mkdirs()
        val out = ByteArrayOutputStream()
        out.write(QUEUE_FORMAT)
        Bincode.writeVec(out, items) { o, b -> Bincode.writeBytes(o, b) }
        val tmp = File(file.path + ".tmp")
        tmp.writeBytes(out.toByteArray())
        if (!tmp.renameTo(file)) tmp.delete()
    }
}
