package dev.istmo.plugins.share

import android.app.Activity
import android.content.ComponentName
import android.content.ContentResolver
import android.content.Context
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
import java.io.InputStream
import java.io.OutputStream
import java.util.UUID
import java.util.concurrent.Executors

/**
 * Entry point for content other apps share into this one.
 *
 * The plugin manifest declares this activity **not exported** and
 * without intent filters, so nothing can reach it until the app opts
 * in with an `activity-alias` in its own `AndroidManifest.xml`:
 *
 * (Concrete mime types shown below; Android also accepts wildcards
 * such as `text/&#42;` or `image/&#42;` — spelled out here because
 * Kotlin's block-comment tokenizer nests on the raw `/&#42;` sequence.)
 *
 * ```xml
 * <activity-alias
 *     android:name="dev.istmo.plugins.share.ShareTarget"
 *     android:targetActivity="dev.istmo.plugins.share.ShareReceiverActivity"
 *     android:exported="true">
 *   <intent-filter>
 *     <action android:name="android.intent.action.SEND" />
 *     <action android:name="android.intent.action.SEND_MULTIPLE" />
 *     <category android:name="android.intent.category.DEFAULT" />
 *     <data android:mimeType="text/plain" />
 *     <data android:mimeType="image/png" />
 *   </intent-filter>
 * </activity-alias>
 * ```
 *
 * The alias is an element the app owns outright, so the manifest merger
 * never has to reconcile the app's attributes with the plugin's (the
 * plugin manifest is merged with the highest priority, which would
 * otherwise win every conflict). The alias name is fixed: direct-share
 * shortcuts target it.
 *
 * Each share is copied into `cacheDir/istmo-share/incoming/<uuid>/`
 * while the sender's read grant is still valid, published on the
 * `istmo.share.incoming` early-event queue (buffered until Rust
 * subscribes), and the app's launcher activity is brought to front.
 * Entries older than [INCOMING_RETENTION_MS] are pruned first.
 *
 * Shared streams are untrusted: only `content://` URIs served by another
 * app are copied (see [isForeignContent]), files larger than
 * [maxIncomingFileBytes] are skipped, and display names are sanitised
 * before they name anything on disk.
 */
class ShareReceiverActivity : Activity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val shareIntent = intent
        if (shareIntent == null || !isShareAction(shareIntent.action)) {
            finish()
            return
        }
        val referrer = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.LOLLIPOP_MR1) referrer?.host else null
        IO.execute {
            try {
                pruneIncoming(File(cacheDir, INCOMING_DIR))
                publish(read(shareIntent, referrer))
            } finally {
                runOnUiThread {
                    openApp()
                    finish()
                }
            }
        }
    }

    private fun read(intent: Intent, referrer: String?): IncomingShare {
        val rawText = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()
        val isUrl = rawText != null && Patterns.WEB_URL.matcher(rawText.trim()).matches()
        val dir = File(cacheDir, "$INCOMING_DIR/${UUID.randomUUID()}")
        val files = streams(intent).mapNotNull { copy(it, dir) }
        val shortcutId = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            intent.getStringExtra(Intent.EXTRA_SHORTCUT_ID)
        } else {
            null
        }
        return IncomingShare(
            text = if (isUrl) null else rawText,
            url = if (isUrl) rawText?.trim() else null,
            subject = intent.getStringExtra(Intent.EXTRA_SUBJECT),
            files = files,
            sourceApp = referrer,
            receivedAtMs = System.currentTimeMillis().toULong(),
            targetId = shortcutId?.removePrefix(ShareBackendImpl.SHORTCUT_PREFIX),
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
        if (!isForeignContent(uri)) return null
        var name = uri.lastPathSegment ?: "shared"
        contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) {
                val idx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                if (idx >= 0 && !cursor.isNull(idx)) name = cursor.getString(idx)
            }
        }
        // The provider chooses the display name: it must not steer the copy
        // (or whatever the app later does with the name) out of `dir`.
        val safeName = ShareBackendImpl.sanitize(name)
        if (!dir.isDirectory && !dir.mkdirs()) return null
        val dest = File(dir, safeName)
        if (dest.canonicalFile.parentFile != dir.canonicalFile) return null
        return try {
            val input = contentResolver.openInputStream(uri) ?: return null
            val copied = input.use { src -> dest.outputStream().use { copyCapped(src, it, maxIncomingFileBytes) } }
            if (copied == null) {
                dest.delete()
                return null
            }
            IncomingFile(
                path = dest.absolutePath,
                name = safeName,
                mimeType = contentResolver.getType(uri) ?: ShareBackendImpl.guessMime(safeName),
                size = copied.toULong(),
            )
        } catch (_: Exception) {
            // Unreadable stream (revoked grant, provider crash): skip the
            // file, keep the rest of the share.
            dest.delete()
            null
        }
    }

    /**
     * Guards against "Dirty Stream" shares: a sender hands over a URI that
     * makes this app read its own private data (a `file://` path into the
     * app's sandbox, or a `content://` URI of one of the app's own
     * providers) and the copy then lands somewhere the sender can reach.
     * Only `content://` URIs served by another package are accepted.
     */
    private fun isForeignContent(uri: Uri): Boolean {
        if (uri.scheme != ContentResolver.SCHEME_CONTENT) return false
        val authority = uri.authority ?: return false
        val owner = try {
            packageManager.resolveContentProvider(authority, 0)?.packageName
        } catch (_: Exception) {
            null
        } ?: return false
        return owner != packageName
    }

    /** Copy at most [limit] bytes; `null` when the stream is longer. */
    private fun copyCapped(src: InputStream, dst: OutputStream, limit: Long): Long? {
        val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
        var total = 0L
        while (true) {
            val read = src.read(buffer)
            if (read < 0) return total
            total += read
            if (total > limit) return null
            dst.write(buffer, 0, read)
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
        /** The opt-in alias the app declares; direct-share shortcuts target it. */
        const val ALIAS_CLASS = "dev.istmo.plugins.share.ShareTarget"

        /** Mirrors `istmo_share::INCOMING_CHANNEL`. */
        const val INCOMING_CHANNEL = "istmo.share.incoming"

        /** Mirrors `istmo_share::INCOMING_QUEUE_CAPACITY`. */
        const val INCOMING_QUEUE_CAPACITY = 8

        /** Mirrors `istmo_share::INCOMING_RETENTION` (7 days). */
        const val INCOMING_RETENTION_MS = 7L * 24 * 60 * 60 * 1000

        private const val INCOMING_DIR = "istmo-share/incoming"

        /** Default of [maxIncomingFileBytes]: 512 MiB. */
        const val DEFAULT_MAX_INCOMING_FILE_BYTES = 512L * 1024 * 1024

        /**
         * Largest file a share may copy in; longer streams are dropped from
         * the share. Set it before shares arrive (e.g. in `Application.onCreate`).
         */
        @JvmStatic
        @Volatile
        var maxIncomingFileBytes: Long = DEFAULT_MAX_INCOMING_FILE_BYTES

        private val IO = Executors.newSingleThreadExecutor()

        /** `true` when the app declared the [ALIAS_CLASS] activity-alias. */
        fun isReceiveEnabled(context: Context): Boolean = try {
            context.packageManager.getActivityInfo(ComponentName(context.packageName, ALIAS_CLASS), 0)
            true
        } catch (_: PackageManager.NameNotFoundException) {
            false
        }

        internal fun pruneIncoming(root: File) {
            val cutoff = System.currentTimeMillis() - INCOMING_RETENTION_MS
            root.listFiles()?.filter { it.lastModified() < cutoff }?.forEach { it.deleteRecursively() }
        }
    }
}
