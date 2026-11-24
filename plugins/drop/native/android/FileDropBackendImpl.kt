package dev.istmo.plugins.drop

import android.app.Activity
import android.content.ClipDescription
import android.content.ContentResolver
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.provider.OpenableColumns
import android.view.DragEvent
import android.view.View
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
 * none of the accepted MIME types is refused when it starts. While one
 * the window takes is over it, `DragState.Over` goes on the
 * `istmo.drop.drag` channel (with the position, a few dozen times a
 * second at most) so the app can show it; `Receiving` while the files are
 * copied, `Idle` after.
 *
 * On drop the backend takes the drag's read permission
 * (`requestDragAndDropPermissions`), copies every file of an accepted type
 * into `cacheDir/istmo-drop/incoming/<uuid>/` on a worker thread, releases
 * the permission, and publishes one `DroppedFiles` on `istmo.drop.files`.
 * Files of other types, above the size limit, unreadable, or not files at
 * all (text, links) come back as skipped. Only content other apps
 * provide is read: a `file://` URI or one of this app's own providers is
 * refused, so a drag cannot make the app hand its private files to Rust
 * as if they came from outside.
 *
 * The copies are the app's: Rust deletes them with `DropInbox::cleanup`;
 * leftovers older than a day go when the backend is built. The drag's
 * permission dies with its activity: files still copying when it is
 * destroyed come back unreadable.
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
    activity: Activity,
) : FileDropBackend, IstmoActivityBound {

    private val context = activity.applicationContext
    private val packageName = context.packageName
    private val resolver: ContentResolver = context.contentResolver
    private val incomingRoot = File(context.cacheDir, INCOMING_DIR)
    private val main = Handler(Looper.getMainLooper())
    private val codecs = FileDropCodecsImpl()

    /** Main thread only, like [options]. */
    private var hostRef = WeakReference(activity)

    /** What the window takes; `null` while not listening. Main thread only. */
    private var options: DropOptions? = null

    /** When the last `Over` position went out, for throttling. */
    private var lastMove = 0L

    init {
        // Copies a crashed or killed process left behind.
        Thread({ pruneOld() }, "istmo-drop-prune").start()
    }

    override fun attachActivity(activity: Activity) {
        val previous = hostRef.get()
        if (previous === activity) return
        previous?.let { detach(it) }
        hostRef = WeakReference(activity)
        // Called from `onCreate`, possibly before `setContentView` and the
        // theme are set: touching the decor view now would build it with
        // the wrong theme. A post runs once `onCreate` is done.
        main.post {
            if (options != null && hostRef.get() === activity) attach(activity)
        }
    }

    override suspend fun listen(options: DropOptions) {
        main.post {
            this.options = options
            hostRef.get()?.let { attach(it) }
        }
    }

    override suspend fun stop() {
        main.post {
            options = null
            hostRef.get()?.let { detach(it) }
            publishState(DragState.Idle)
        }
    }

    private fun attach(activity: Activity) {
        activity.window?.decorView?.setOnDragListener(listener)
    }

    private fun detach(activity: Activity) {
        // `peekDecorView`: never build a decor view just to clear it.
        activity.window?.peekDecorView()?.setOnDragListener(null)
    }

    private val listener = View.OnDragListener { view, event ->
        val accepting = options ?: return@OnDragListener false
        val density = view.resources.displayMetrics.density
        when (event.action) {
            // `true` asks for the rest of the drag; `false` refuses it.
            DragEvent.ACTION_DRAG_STARTED -> accepts(event.clipDescription, accepting)
            DragEvent.ACTION_DRAG_ENTERED -> {
                publishState(over(event, density))
                true
            }
            DragEvent.ACTION_DRAG_LOCATION -> {
                val now = SystemClock.uptimeMillis()
                if (now - lastMove >= MOVE_INTERVAL_MS) {
                    lastMove = now
                    publishState(over(event, density))
                }
                true
            }
            DragEvent.ACTION_DRAG_EXITED -> {
                publishState(DragState.Idle)
                true
            }
            DragEvent.ACTION_DRAG_ENDED -> {
                // A drop already moved the state on to `Receiving`.
                if (!event.result) publishState(DragState.Idle)
                true
            }
            DragEvent.ACTION_DROP -> onDrop(view, event, accepting, density)
            else -> true
        }
    }

    private fun over(event: DragEvent, density: Float): DragState {
        // `clipDescription` only; the items are not readable before the drop.
        val count = event.clipData?.itemCount ?: 0
        return DragState.Over(
            DragOver(count.toUInt(), DropPoint(event.x / density, event.y / density), null),
        )
    }

    private fun accepts(description: ClipDescription?, accepting: DropOptions): Boolean {
        description ?: return false
        if (accepting.mimeTypes.isEmpty()) {
            // Text or links alone have no file to copy.
            return (0 until description.mimeTypeCount).any {
                description.getMimeType(it) !in NOT_FILES
            }
        }
        return accepting.mimeTypes.any { description.hasMimeType(it) }
    }

    private fun onDrop(view: View, event: DragEvent, accepting: DropOptions, density: Float): Boolean {
        val activity = hostRef.get() ?: return false
        // Only valid while the drop is being handled, and for this activity.
        val permissions = activity.requestDragAndDropPermissions(event)
        val clip = event.clipData ?: run {
            permissions?.release()
            return false
        }
        val position = DropPoint(event.x / density, event.y / density)
        val items = (0 until clip.itemCount).map { clip.getItemAt(it) }
        val limit = accepting.maxBytes
            .takeIf { it != 0uL }
            ?.coerceAtMost(Long.MAX_VALUE.toULong())
            ?.toLong()
            ?: DEFAULT_MAX_BYTES
        publishState(DragState.Receiving(items.size.toUInt()))
        Thread({
            val files = mutableListOf<DroppedFile>()
            val skipped = mutableListOf<SkippedFile>()
            val folder = File(incomingRoot, UUID.randomUUID().toString())
            try {
                for (item in items) {
                    val uri = item.uri
                    when {
                        uri == null -> skipped += SkippedFile("", SkipReason.NotAFile)
                        !isForeignContent(uri) -> skipped += SkippedFile(
                            uri.lastPathSegment?.let(::safeName) ?: "",
                            if (uri.scheme == ContentResolver.SCHEME_CONTENT || uri.scheme == "file") {
                                SkipReason.Unreadable
                            } else {
                                // http(s) links and the like.
                                SkipReason.NotAFile
                            },
                        )
                        else -> copy(uri, folder, limit, accepting.mimeTypes, files, skipped)
                    }
                }
            } catch (_: Exception) {
                // A provider may throw anything; what was copied still goes.
            } finally {
                main.post { permissions?.release() }
            }
            if (files.isNotEmpty() || skipped.isNotEmpty()) {
                val dropped = DroppedFiles(
                    files = files,
                    skipped = skipped,
                    position = position,
                    receivedAtMs = System.currentTimeMillis().toULong(),
                    windowId = null,
                )
                val out = ByteArrayOutputStream()
                codecs.writeDroppedFiles(out, dropped)
                IstmoRuntime.publishEarlyQueue(CHANNEL, CAPACITY, out.toByteArray())
            }
            main.post { publishState(DragState.Idle) }
        }, "istmo-drop-copy").start()
        return true
    }

    private fun copy(
        uri: Uri,
        folder: File,
        limit: Long,
        accepted: List<String>,
        files: MutableList<DroppedFile>,
        skipped: MutableList<SkippedFile>,
    ) {
        val (name, declaredSize) = describe(uri)
        val type = try {
            resolver.getType(uri)
        } catch (_: Exception) {
            null
        }
        // The drag was taken for one accepted type; every file is checked.
        if (accepted.isNotEmpty() && accepted.none { ClipDescription.compareMimeTypes(type ?: "", it) }) {
            skipped += SkippedFile(name, SkipReason.WrongType)
            return
        }
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
            var tooLarge = false
            input.use { stream ->
                target.outputStream().use { output ->
                    val buffer = ByteArray(BUFFER)
                    while (true) {
                        val read = stream.read(buffer)
                        if (read < 0) break
                        copied += read
                        // The provider may report no size, or a wrong one.
                        if (copied > limit) {
                            tooLarge = true
                            break
                        }
                        output.write(buffer, 0, read)
                    }
                }
            }
            if (tooLarge) {
                target.delete()
                skipped += SkippedFile(name, SkipReason.TooLarge)
                return
            }
            files += DroppedFile(
                path = target.absolutePath,
                displayName = name,
                mimeType = type,
                size = copied.toULong(),
            )
        } catch (_: Exception) {
            target.delete()
            skipped += SkippedFile(name, SkipReason.Unreadable)
        }
    }

    /**
     * Content another app provides. Not `file://` (a path the other side
     * picked, possibly inside this app) and not this app's own providers.
     */
    private fun isForeignContent(uri: Uri): Boolean {
        if (uri.scheme != ContentResolver.SCHEME_CONTENT) return false
        // A cross-user URI carries its user id as `<user>@<authority>`.
        val authority = uri.authority?.substringAfterLast('@')?.takeIf { it.isNotEmpty() } ?: return false
        val own = ownAuthorities()
        if (own != null && authority in own) return false
        val owner = try {
            context.packageManager.resolveContentProvider(authority, 0)?.packageName
        } catch (_: Exception) {
            null
        } ?: return own != null
        return owner != packageName
    }

    /** Authorities of every provider this package declares; `null` if unknown. */
    private fun ownAuthorities(): Set<String>? = try {
        val manager = context.packageManager
        val info = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            manager.getPackageInfo(
                packageName,
                PackageManager.PackageInfoFlags.of(PackageManager.GET_PROVIDERS.toLong()),
            )
        } else {
            @Suppress("DEPRECATION")
            manager.getPackageInfo(packageName, PackageManager.GET_PROVIDERS)
        }
        info.providers.orEmpty()
            .flatMap { it.authority?.split(';').orEmpty() }
            .toSet()
    } catch (_: Exception) {
        null
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
        } catch (_: Exception) {
            // Some providers refuse queries; the name falls back below.
        }
        return safeName(name ?: uri.lastPathSegment ?: "file") to size
    }

    /**
     * `name` without path separators, control characters or bidi
     * overrides, short enough in UTF-8 bytes for any file system once a
     * " (n)" suffix is added, its extension kept.
     */
    private fun safeName(name: String): String {
        val cleaned = name
            .filterNot { it.isISOControl() || it in BIDI_CONTROLS }
            .replace('/', '_')
            .replace('\\', '_')
            .trim()
            .trimStart('.')
            .ifEmpty { "file" }
        if (cleaned.toByteArray().size <= MAX_NAME_BYTES) return cleaned
        val dot = cleaned.lastIndexOf('.')
        val ext = if (dot > 0 && cleaned.length - dot <= MAX_EXTENSION) cleaned.substring(dot) else ""
        val stem = cleaned.removeSuffix(ext)
        val budget = MAX_NAME_BYTES - ext.toByteArray().size
        val kept = StringBuilder()
        var bytes = 0
        var at = 0
        while (at < stem.length) {
            val codePoint = stem.codePointAt(at)
            val size = String(Character.toChars(codePoint)).toByteArray().size
            if (bytes + size > budget) break
            kept.appendCodePoint(codePoint)
            bytes += size
            at += Character.charCount(codePoint)
        }
        return kept.toString() + ext
    }

    private fun uniqueFile(folder: File, name: String): File {
        var candidate = File(folder, name)
        var n = 1
        val dot = name.lastIndexOf('.')
        val stem = if (dot > 0) name.substring(0, dot) else name
        val ext = if (dot > 0) name.substring(dot) else ""
        while (candidate.exists()) {
            candidate = File(folder, "$stem ($n)$ext")
            n++
        }
        return candidate
    }

    /** Main thread. */
    private fun publishState(state: DragState) {
        val out = ByteArrayOutputStream()
        codecs.writeDragState(out, state)
        IstmoRuntime.publishEarlyLatest(DRAG_CHANNEL, out.toByteArray())
    }

    private fun pruneOld() {
        val cutoff = System.currentTimeMillis() - MAX_AGE_MS
        incomingRoot.listFiles()?.forEach { folder ->
            if (folder.lastModified() < cutoff) folder.deleteRecursively()
        }
    }

    private companion object {
        // Same as `DROP_CHANNEL`, `DROP_QUEUE_CAPACITY`, `DRAG_CHANNEL`,
        // `DROP_DIR` and `DEFAULT_MAX_BYTES` in the Rust crate.
        const val CHANNEL = "istmo.drop.files"
        const val CAPACITY = 8
        const val DRAG_CHANNEL = "istmo.drop.drag"
        const val INCOMING_DIR = "istmo-drop/incoming"
        const val DEFAULT_MAX_BYTES = 512L * 1024 * 1024
        const val MAX_AGE_MS = 24L * 60 * 60 * 1000
        const val MOVE_INTERVAL_MS = 33L
        const val BUFFER = 64 * 1024
        const val MAX_NAME_BYTES = 200
        const val MAX_EXTENSION = 16
        val NOT_FILES = setOf(
            ClipDescription.MIMETYPE_TEXT_PLAIN,
            ClipDescription.MIMETYPE_TEXT_HTML,
            ClipDescription.MIMETYPE_TEXT_INTENT,
        )
        val BIDI_CONTROLS = setOf(
            '‪', '‫', '‬', '‭', '‮',
            '⁦', '⁧', '⁨', '⁩', '‎', '‏',
        )
    }
}
