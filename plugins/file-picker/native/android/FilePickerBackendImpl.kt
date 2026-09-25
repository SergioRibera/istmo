package dev.istmo.plugins.filepicker

import android.content.ContentResolver
import android.content.Intent
import android.net.Uri
import android.provider.OpenableColumns
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResult
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.FilePickerBackend
import dev.istmo.runtime.FilePickerError
import dev.istmo.runtime.HandleReleaser
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.NativeHandleId
import dev.istmo.runtime.PickConfig
import dev.istmo.runtime.PickedFile
import dev.istmo.runtime.RawFileHandle
import dev.istmo.runtime.SaveConfig
import java.util.concurrent.ConcurrentHashMap
import kotlin.coroutines.resume
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * Reference SAF backend for `istmo.file_picker`.
 *
 * `ActivityResultLauncher`s must be registered before the host `Activity`
 * reaches `STARTED`, so this class *must* be constructed inside
 * `Activity.onCreate` and held for the activity's lifetime. Register it
 * with the plugin registry once during onCreate:
 *
 * ```kotlin
 * override fun onCreate(savedInstanceState: Bundle?) {
 *     super.onCreate(savedInstanceState)
 *     val picker = FilePickerBackendImpl(this)
 *     IstmoRuntime.registerHandler("istmo.file_picker", picker)
 * }
 * ```
 *
 * Auto-registration is disabled in `istmo.toml` because the constructor
 * needs an `Activity`, not a plain `Context`.
 */
class FilePickerBackendImpl(
    private val activity: ComponentActivity,
) : FilePickerBackend, HandleReleaser {

    private val resolver: ContentResolver = activity.contentResolver
    private val files = ConcurrentHashMap<Long, Uri>()

    /** Serialises picker launches — SAF only tolerates one modal at a time. */
    private val launchMutex = Mutex()
    private var pending: CancellableContinuation<ActivityResult>? = null

    private val openDocumentLauncher: ActivityResultLauncher<Intent> =
        activity.registerForActivityResult(
            ActivityResultContracts.StartActivityForResult()
        ) { result -> resumePending(result) }

    private val createDocumentLauncher: ActivityResultLauncher<Intent> =
        activity.registerForActivityResult(
            ActivityResultContracts.StartActivityForResult()
        ) { result -> resumePending(result) }

    // ------------------------------------------------------------- FilePicker impl

    override suspend fun pick_file(config: PickConfig): PickedFile? {
        val intent = buildOpenIntent(config, multi = false)
        val result = launch(openDocumentLauncher, intent)
        val uri = result.data?.data ?: return null
        takePersistable(uri, config.filter.mime_types.contains("*/*") || config.filter.mime_types.isEmpty())
        return registerAndDescribe(uri)
    }

    override suspend fun pick_files(config: PickConfig): List<PickedFile> {
        val intent = buildOpenIntent(config, multi = config.allow_multiple)
        val result = launch(openDocumentLauncher, intent)
        val data = result.data ?: return emptyList()
        val clip = data.clipData
        val uris: List<Uri> = if (clip != null) {
            List(clip.itemCount) { clip.getItemAt(it).uri }
        } else {
            listOfNotNull(data.data)
        }
        return uris.map { uri ->
            takePersistable(uri, writable = false)
            registerAndDescribe(uri)
        }
    }

    override suspend fun save_file(config: SaveConfig): PickedFile? {
        val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = config.mime_type ?: "application/octet-stream"
            config.suggested_name?.let { putExtra(Intent.EXTRA_TITLE, it) }
        }
        val result = launch(createDocumentLauncher, intent)
        val uri = result.data?.data ?: return null
        takePersistable(uri, writable = true)
        return registerAndDescribe(uri)
    }

    override suspend fun open_read(file: NativeHandleId): RawFileHandle {
        val uri = files[file] ?: throw BackendException(FilePickerError.NotFound("handle $file"))
        val pfd = resolver.openFileDescriptor(uri, "r")
            ?: throw BackendException(FilePickerError.Io("openFileDescriptor returned null for $uri"))
        return RawFileHandle(pfd.detachFd().toLong())
    }

    override suspend fun open_write(file: NativeHandleId): RawFileHandle {
        val uri = files[file] ?: throw BackendException(FilePickerError.NotFound("handle $file"))
        val pfd = resolver.openFileDescriptor(uri, "wt")
            ?: throw BackendException(FilePickerError.Io("openFileDescriptor returned null for $uri"))
        return RawFileHandle(pfd.detachFd().toLong())
    }

    override suspend fun path(file: NativeHandleId): String? {
        // SAF `content://` URIs are not filesystem paths since API 29.
        // Return null to force callers into `PickedFileReader` / a cache
        // copy fallback when a real path is required.
        if (!files.containsKey(file)) throw BackendException(FilePickerError.NotFound("handle $file"))
        return null
    }

    // ------------------------------------------------------------- HandleReleaser

    override fun onReleaseNativeHandle(handle: NativeHandleId) {
        val uri = files.remove(handle) ?: return
        try {
            val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
            resolver.releasePersistableUriPermission(uri, flags)
        } catch (_: SecurityException) {
            // Permission may already have been dropped by the system.
        }
        IstmoRuntime.forgetHandle(handle)
    }

    // ------------------------------------------------------------- helpers

    private fun buildOpenIntent(config: PickConfig, multi: Boolean): Intent {
        val mimes = config.filter.mime_types.ifEmpty { listOf("*/*") }
        return Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = mimes.first()
            if (mimes.size > 1) {
                putExtra(Intent.EXTRA_MIME_TYPES, mimes.toTypedArray())
            }
            if (multi) putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
        }
    }

    private suspend fun launch(
        launcher: ActivityResultLauncher<Intent>,
        intent: Intent,
    ): ActivityResult = launchMutex.withLock {
        suspendCancellableCoroutine { cont ->
            pending = cont
            try {
                launcher.launch(intent)
            } catch (t: Throwable) {
                pending = null
                cont.resumeWith(Result.failure(BackendException(t.message ?: "launch failed")))
            }
        }
    }

    private fun resumePending(result: ActivityResult) {
        val cont = pending ?: return
        pending = null
        cont.resume(result)
    }

    private fun takePersistable(uri: Uri, writable: Boolean) {
        val flags = if (writable) {
            Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
        } else {
            Intent.FLAG_GRANT_READ_URI_PERMISSION
        }
        try {
            resolver.takePersistableUriPermission(uri, flags)
        } catch (_: SecurityException) {
            // Some providers (e.g. transient in-memory ones) do not
            // support persistence; the URI still works for the current
            // process, so keep going.
        }
    }

    private fun registerAndDescribe(uri: Uri): PickedFile {
        val id = IstmoRuntime.allocHandleId(HANDLE_KIND)
        files[id] = uri
        IstmoRuntime.registerReleaser(HANDLE_KIND, this)
        val (name, size, mime) = describe(uri)
        return PickedFile(display_name = name, mime_type = mime, size = size, handle = id)
    }

    private fun describe(uri: Uri): Triple<String, Long?, String?> {
        val mime = resolver.getType(uri)
        var name: String = uri.lastPathSegment ?: ""
        var size: Long? = null
        resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)
            ?.use { cursor ->
                if (cursor.moveToFirst()) {
                    val nameIdx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    if (nameIdx >= 0 && !cursor.isNull(nameIdx)) name = cursor.getString(nameIdx)
                    val sizeIdx = cursor.getColumnIndex(OpenableColumns.SIZE)
                    if (sizeIdx >= 0 && !cursor.isNull(sizeIdx)) size = cursor.getLong(sizeIdx)
                }
            }
        return Triple(name, size, mime)
    }

    companion object {
        private const val HANDLE_KIND = "istmo.file_picker.file_ref"
    }
}
