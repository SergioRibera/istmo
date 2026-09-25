package dev.istmo.plugins.share

import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.OpenableColumns
import android.util.Patterns
import dev.istmo.runtime.IncomingFile
import dev.istmo.runtime.IncomingShare
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.ShareCodecsImpl
import java.io.ByteArrayOutputStream
import java.io.File
import java.util.UUID
import java.util.concurrent.Executors

/**
 * Entry point for content other apps share into this one.
 *
 * Declared by the plugin manifest without intent filters, so the app is
 * not a share target until it opts in by adding, in its own
 * `AndroidManifest.xml`:
 *
 * ```xml
 * <application>
 *   <meta-data android:name="dev.istmo.share.RECEIVE" android:value="true" />
 *   <activity android:name="dev.istmo.plugins.share.ShareReceiverActivity" android:exported="true">
 *     <intent-filter>
 *       <action android:name="android.intent.action.SEND" />
 *       <action android:name="android.intent.action.SEND_MULTIPLE" />
 *       <category android:name="android.intent.category.DEFAULT" />
 *       <data android:mimeType="text/*" />
 *       <data android:mimeType="image/*" />
 *     </intent-filter>
 *   </activity>
 * </application>
 * ```
 *
 * Without the `RECEIVE` meta-data the activity finishes immediately.
 *
 * Each share is copied into `cacheDir/istmo-share/incoming/<uuid>/`
 * while the sender's read grant is still valid, published on the
 * `istmo.share.incoming` early-event queue (buffered until Rust
 * subscribes), and the app's launcher activity is brought to front.
 */
class ShareReceiverActivity : Activity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val shareIntent = intent
        if (!receiveEnabled() || shareIntent == null || !isShareAction(shareIntent.action)) {
            finish()
            return
        }
        val referrer = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.LOLLIPOP_MR1) referrer?.host else null
        IO.execute {
            try {
                publish(read(shareIntent, referrer))
            } finally {
                runOnUiThread {
                    openApp()
                    finish()
                }
            }
        }
    }

    private fun receiveEnabled(): Boolean {
        val info = try {
            packageManager.getApplicationInfo(packageName, PackageManager.GET_META_DATA)
        } catch (_: PackageManager.NameNotFoundException) {
            return false
        }
        return info.metaData?.getBoolean(RECEIVE_META_DATA, false) == true
    }

    private fun read(intent: Intent, referrer: String?): IncomingShare {
        val rawText = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()
        val isUrl = rawText != null && Patterns.WEB_URL.matcher(rawText.trim()).matches()
        val dir = File(cacheDir, "istmo-share/incoming/${UUID.randomUUID()}")
        val files = streams(intent).mapNotNull { copy(it, dir) }
        return IncomingShare(
            text = if (isUrl) null else rawText,
            url = if (isUrl) rawText?.trim() else null,
            subject = intent.getStringExtra(Intent.EXTRA_SUBJECT),
            files = files,
            sourceApp = referrer,
            receivedAtMs = System.currentTimeMillis().toULong(),
        )
    }

    private fun streams(intent: Intent): List<Uri> {
        val fromExtras: List<Uri> = if (intent.action == Intent.ACTION_SEND_MULTIPLE) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM, Uri::class.java).orEmpty()
            } else {
                @Suppress("DEPRECATION")
                intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM).orEmpty()
            }
        } else {
            val single = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
            } else {
                @Suppress("DEPRECATION")
                intent.getParcelableExtra(Intent.EXTRA_STREAM)
            }
            listOfNotNull(single)
        }
        if (fromExtras.isNotEmpty()) return fromExtras
        val clip = intent.clipData ?: return emptyList()
        return List(clip.itemCount) { clip.getItemAt(it).uri }.filterNotNull()
    }

    private fun copy(uri: Uri, dir: File): IncomingFile? {
        var name = uri.lastPathSegment ?: "shared"
        contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) {
                val idx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                if (idx >= 0 && !cursor.isNull(idx)) name = cursor.getString(idx)
            }
        }
        if (!dir.isDirectory && !dir.mkdirs()) return null
        val dest = File(dir, ShareBackendImpl.sanitize(name))
        return try {
            val input = contentResolver.openInputStream(uri) ?: return null
            input.use { src -> dest.outputStream().use { src.copyTo(it) } }
            IncomingFile(
                path = dest.absolutePath,
                name = name,
                mimeType = contentResolver.getType(uri) ?: ShareBackendImpl.guessMime(name),
                size = dest.length().toULong(),
            )
        } catch (_: Exception) {
            // Unreadable stream (revoked grant, provider crash): skip the
            // file, keep the rest of the share.
            dest.delete()
            null
        }
    }

    private fun publish(share: IncomingShare) {
        val out = ByteArrayOutputStream()
        ShareCodecsImpl().writeIncomingShare(out, share)
        IstmoRuntime.publishEarlyQueue(INCOMING_CHANNEL, INCOMING_QUEUE_CAPACITY, out.toByteArray())
    }

    private fun openApp() {
        val launch = packageManager.getLaunchIntentForPackage(packageName) ?: return
        launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_REORDER_TO_FRONT)
        startActivity(launch)
    }

    private fun isShareAction(action: String?): Boolean =
        action == Intent.ACTION_SEND || action == Intent.ACTION_SEND_MULTIPLE

    companion object {
        /** App-level meta-data that opts the app into receiving shares. */
        const val RECEIVE_META_DATA = "dev.istmo.share.RECEIVE"

        /** Mirrors `istmo_share::INCOMING_CHANNEL`. */
        const val INCOMING_CHANNEL = "istmo.share.incoming"

        /** Mirrors `istmo_share::INCOMING_QUEUE_CAPACITY`. */
        const val INCOMING_QUEUE_CAPACITY = 8

        private val IO = Executors.newSingleThreadExecutor()
    }
}
