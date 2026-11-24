package dev.istmo.plugins.drop

import android.app.Activity
import android.content.ClipDescription
import android.content.ContentResolver
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.OpenableColumns
import android.view.DragEvent
import android.view.View
import androidx.activity.ComponentActivity
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.IstmoActivityBound
import dev.istmo.runtime.IstmoRuntime
import java.io.ByteArrayOutputStream
import java.io.File
import java.lang.ref.WeakReference
import java.util.UUID

/**
 * Backend of `istmo.drop` on Android: files dragged from another app
 * (split screen, freeform windows, ChromeOS) onto the app's window.
 *
 * Nothing listens until Rust calls `listen`. Then a drag listener sits
 * on the activity's decor view, under the whole window: the surface the
 * app draws on does not take drags, so they reach it. A drag carrying
 * none of the accepted MIME types is refused when it starts, and the
 * system shows it cannot be dropped here.
 *
 * On drop the backend takes the drag's read permission
 * (`requestDragAndDropPermissions`), copies every file into
 * `cacheDir/istmo-drop/incoming/<uuid>/` on a worker thread, releases the
 * permission, and publishes one `DroppedFiles` on the `istmo.drop.files`
 * queue. Files above the size limit, unreadable ones and plain text are
 * listed as skipped. The copies are the app's: Rust deletes them with
 * `DropInbox::cleanup`; leftovers older than a day go at the next start.
 *
 * The backend outlives its activity ([IstmoActivityBound]): the
 * generated registry builds it once and hands it each new activity, and
 * the listener moves along. Registering it by hand:
 *
 * ```kotlin
 * val backend = FileDropBackendImpl(this)
 * IstmoRuntime.registerHandler(
 *     FileDropDispatcher.PLUGIN_ID,
 *     FileDropDispatcher(backend, FileDropCodecsImpl()),
 * )
 * ```
 */
class FileDropBackendImpl(
    activity: ComponentActivity,
) : FileDropBackend, IstmoActivityBound {

    private val context = activity.applicationContext
    private val resolver: ContentResolver = context.contentResolver
    private val incomingRoot = File(context.cacheDir, INCOMING_DIR)
    private val main = Handler(Looper.getMainLooper())
    private val codecs = FileDropCodecsImpl()

    private var hostRef = WeakReference(activity)

    @Volatile
    private var options: DropOptions? = null

    init {
        // Copies a crashed or killed process left behind.
        Thread({ pruneOld() }, "istmo-drop-prune").start()
    }

    override fun attachActivity(activity: Activity) {
        val previous = hostRef.get()
        if (previous === activity) return
        previous?.let { detach(it) }
        hostRef = WeakReference(activity as ComponentActivity)
        if (options != null) attach(activity)
    }

    override suspend fun listen(options: DropOptions) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.N) {
            throw BackendException(DropError.Unsupported)
        }
        this.options = options
        main.post { hostRef.get()?.let { attach(it) } }
    }

    override suspend fun stop() {
        options = null
        main.post { hostRef.get()?.let { detach(it) } }
    }

    private fun attach(activity: Activity) {
        activity.window?.decorView?.setOnDragListener(listener)
    }

    private fun detach(activity: Activity) {
        activity.window?.decorView?.setOnDragListener(null)
    }

    private val listener = View.OnDragListener { view, event ->
        val accepting = options ?: return@OnDragListener false
        when (event.action) {
            // `true` asks for the rest of the drag; `false` refuses it.
            DragEvent.ACTION_DRAG_STARTED -> accepts(event.clipDescription, accepting)
            DragEvent.ACTION_DROP -> {
                onDrop(view, event, accepting)
                true
            }
            else -> true
        }
    }

    private fun accepts(description: ClipDescription?, accepting: DropOptions): Boolean {
        description ?: return false
        if (accepting.mimeTypes.isEmpty()) {
            return (0 until description.mimeTypeCount).any {
                description.getMimeType(it) != ClipDescription.MIMETYPE_TEXT_PLAIN
            }
        }
        return accepting.mimeTypes.any { description.hasMimeType(it) }
    }

    private fun onDrop(view: View, event: DragEvent, accepting: DropOptions) {
        val activity = hostRef.get() ?: return
        // Only valid while the drop is being handled, and for this activity.
        val permissions = activity.requestDragAndDropPermissions(event)
        val clip = event.clipData ?: run {
            permissions?.release()
            return
        }
        val density = view.resources.displayMetrics.density
        val position = DropPoint(event.x / density, event.y / density)
        val items = (0 until clip.itemCount).map { clip.getItemAt(it) }
        val limit = if (accepting.maxBytes == 0uL) DEFAULT_MAX_BYTES else accepting.maxBytes.toLong()
        Thread({
            val files = mutableListOf<DroppedFile>()
            val skipped = mutableListOf<SkippedFile>()
            val folder = File(incomingRoot, UUID.randomUUID().toString())
            try {
                for (item in items) {
                    val uri = item.uri
                    if (uri == null) {
                        skipped += SkippedFile(item.text?.toString()?.take(40) ?: "", SkipReason.NotAFile)
                        continue
                    }
                    copy(uri, folder, limit, files, skipped)
                }
            } finally {
                main.post { permissions?.release() }
            }
            if (files.isEmpty() && skipped.isEmpty()) return@Thread
            val dropped = DroppedFiles(
                files = files,
                skipped = skipped,
                position = position,
                receivedAtMs = System.currentTimeMillis().toULong(),
            )
            val out = ByteArrayOutputStream()
            codecs.writeDroppedFiles(out, dropped)
            IstmoRuntime.publishEarlyQueue(CHANNEL, CAPACITY, out.toByteArray())
        }, "istmo-drop-copy").start()
    }

    private fun copy(
        uri: Uri,
        folder: File,
        limit: Long,
        files: MutableList<DroppedFile>,
        skipped: MutableList<SkippedFile>,
    ) {
        val (name, declaredSize) = describe(uri)
        if (declaredSize != null && declaredSize > limit) {
            skipped += SkippedFile(name, SkipReason.TooLarge)
            return
        }
        folder.mkdirs()
        val target = uniqueFile(folder, name)
        try {
            val input = resolver.openInputStream(uri)
            if (input == null) {
                skipped += SkippedFile(name, SkipReason.Unreadable)
                return
            }
            var copied = 0L
            input.use { stream ->
                target.outputStream().use { output ->
                    val buffer = ByteArray(BUFFER)
                    while (true) {
                        val read = stream.read(buffer)
                        if (read < 0) break
                        copied += read
                        // The provider may report no size, or a wrong one.
                        if (copied > limit) {
                            output.close()
                            target.delete()
                            skipped += SkippedFile(name, SkipReason.TooLarge)
                            return
                        }
                        output.write(buffer, 0, read)
                    }
                }
            }
            files += DroppedFile(
                path = target.absolutePath,
                displayName = name,
                mimeType = resolver.getType(uri),
                size = copied.toULong(),
            )
        } catch (_: SecurityException) {
            target.delete()
            skipped += SkippedFile(name, SkipReason.Unreadable)
        } catch (_: java.io.IOException) {
            target.delete()
            skipped += SkippedFile(name, SkipReason.Unreadable)
        }
    }

    /** Display name and size the provider reports, when it does. */
    private fun describe(uri: Uri): Pair<String, Long?> {
        var name: String? = null
        var size: Long? = null
        try {
            resolver.query(
                uri,
                arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE),
                null,
                null,
                null,
            )?.use { cursor ->
                if (cursor.moveToFirst()) {
                    val nameAt = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    val sizeAt = cursor.getColumnIndex(OpenableColumns.SIZE)
                    if (nameAt >= 0 && !cursor.isNull(nameAt)) name = cursor.getString(nameAt)
                    if (sizeAt >= 0 && !cursor.isNull(sizeAt)) size = cursor.getLong(sizeAt)
                }
            }
        } catch (_: RuntimeException) {
            // Some providers refuse queries; the name falls back below.
        }
        return safeName(name ?: uri.lastPathSegment ?: "file") to size
    }

    /** `name` without path separators, control characters or bidi overrides. */
    private fun safeName(name: String): String {
        val cleaned = name
            .filterNot { it.isISOControl() || it in BIDI_CONTROLS }
            .replace('/', '_')
            .replace('\\', '_')
            .trim()
            .trimStart('.')
            .take(MAX_NAME)
        return cleaned.ifEmpty { "file" }
    }

    private fun uniqueFile(folder: File, name: String): File {
        var candidate = File(folder, name)
        var n = 1
        while (candidate.exists()) {
            val dot = name.lastIndexOf('.')
            val stem = if (dot > 0) name.substring(0, dot) else name
            val ext = if (dot > 0) name.substring(dot) else ""
            candidate = File(folder, "$stem ($n)$ext")
            n++
        }
        return candidate
    }

    private fun pruneOld() {
        val cutoff = System.currentTimeMillis() - MAX_AGE_MS
        incomingRoot.listFiles()?.forEach { folder ->
            if (folder.lastModified() < cutoff) folder.deleteRecursively()
        }
    }

    private companion object {
        // Same names as `DROP_CHANNEL`, `DROP_QUEUE_CAPACITY`, `DROP_DIR`
        // and `DEFAULT_MAX_BYTES` in the Rust crate.
        const val CHANNEL = "istmo.drop.files"
        const val CAPACITY = 8
        const val INCOMING_DIR = "istmo-drop/incoming"
        const val DEFAULT_MAX_BYTES = 512L * 1024 * 1024
        const val MAX_AGE_MS = 24L * 60 * 60 * 1000
        const val BUFFER = 64 * 1024
        const val MAX_NAME = 120
        val BIDI_CONTROLS = setOf(
            '‪', '‫', '‬', '‭', '‮',
            '⁦', '⁧', '⁨', '⁩', '‎', '‏',
        )
    }
}
