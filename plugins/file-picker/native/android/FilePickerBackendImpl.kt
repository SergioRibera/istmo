package dev.istmo.plugins.filepicker

import android.content.ContentResolver
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.OpenableColumns
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResult
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
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
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
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
 * `Activity.onCreate` and held for the activity's lifetime. The
 * generated `IstmoPluginRegistry` does exactly that (`istmo.toml` sets
 * `android_backend_arg = "activity"`) when `registerAll` runs from
 * `onCreate`. Registering by hand looks the same:
 *
 * ```kotlin
 * override fun onCreate(savedInstanceState: Bundle?) {
 *     super.onCreate(savedInstanceState)
 *     val backend = FilePickerBackendImpl(this)
 *     val dispatcher = FilePickerDispatcher(backend, FilePickerCodecsImpl())
 *     IstmoRuntime.registerHandler(FilePickerDispatcher.PLUGIN_ID, dispatcher)
 * }
 * ```
 *
 * The dispatcher forwards `releaseNativeHandle` calls to this backend
 * (which implements `HandleReleaser`) so URI permissions drop
 * deterministically when the Rust side lets the handle go.
 *
 * # Activity recreation
 *
 * The launchers belong to the activity that constructed the backend. If
 * that activity is destroyed while a picker is open (e.g. a
 * configuration change without `android:configChanges`), its result can
 * no longer reach this instance: the pending call fails with
 * `FilePickerError.Backend` instead of hanging, and the caller retries
 * against the backend registered by the new activity.
 *
 * # Persisted grants
 *
 * Every URI this backend persists is recorded in its own
 * `SharedPreferences`. Handles never outlive the process, so grants
 * recorded by a previous process are orphans and are released when the
 * next backend is constructed. Grants the app persisted through other
 * code are never touched.
 */
class FilePickerBackendImpl(
    private val activity: ComponentActivity,
) : FilePickerBackend, HandleReleaser {

    private val resolver: ContentResolver = activity.contentResolver
    private val grantLog = activity.applicationContext
        .getSharedPreferences(GRANT_PREFS, Context.MODE_PRIVATE)

    /** Serialises picker launches — SAF only tolerates one modal at a time. */
    private val launchMutex = Mutex()
    private val pending = AtomicReference<CancellableContinuation<ActivityResult>?>(null)

    private val openDocumentLauncher: ActivityResultLauncher<Intent> =
        activity.registerForActivityResult(
            ActivityResultContracts.StartActivityForResult()
        ) { result -> resumePending(result) }

    private val createDocumentLauncher: ActivityResultLauncher<Intent> =
        activity.registerForActivityResult(
            ActivityResultContracts.StartActivityForResult()
        ) { result -> resumePending(result) }

    init {
        if (reconciled.compareAndSet(false, true)) releaseOrphanedGrants()
        activity.lifecycle.addObserver(LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_DESTROY) {
                pending.getAndSet(null)?.resumeWith(
                    Result.failure(
                        BackendException(
                            FilePickerError.Backend("host activity destroyed while the picker was open"),
                        ),
                    ),
                )
            }
        })
    }

    // ------------------------------------------------------------- FilePicker impl

    override suspend fun pick_file(config: PickConfig): PickedFile? {
        val intent = buildOpenIntent(config, multi = false)
        val result = launch(openDocumentLauncher, intent)
        val data = result.data ?: return null
        val uri = data.data ?: return null
        return registerAndDescribe(uri, grantedFlags(data))
    }

    override suspend fun pick_files(config: PickConfig): List<PickedFile> {
        val intent = buildOpenIntent(config, multi = config.allowMultiple)
        val result = launch(openDocumentLauncher, intent)
        val data = result.data ?: return emptyList()
        val flags = grantedFlags(data)
        val clip = data.clipData
        val uris: List<Uri> = if (clip != null) {
            List(clip.itemCount) { clip.getItemAt(it).uri }
        } else {
            listOfNotNull(data.data)
        }
        return uris.map { uri -> registerAndDescribe(uri, flags) }
    }

    override suspend fun save_file(config: SaveConfig): PickedFile? {
        val intent = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = config.mimeType ?: "application/octet-stream"
            config.suggestedName?.let { putExtra(Intent.EXTRA_TITLE, it) }
        }
        val result = launch(createDocumentLauncher, intent)
        val data = result.data ?: return null
        val uri = data.data ?: return null
        return registerAndDescribe(uri, grantedFlags(data))
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

    override fun releaseNativeHandle(handleId: Long) {
        val uri = files.remove(handleId) ?: return
        IstmoRuntime.forgetHandle(handleId)
        val lastRef = synchronized(uriRefs) {
            val left = (uriRefs[uri] ?: 1) - 1
            if (left <= 0) uriRefs.remove(uri) else uriRefs[uri] = left
            left <= 0
        }
        if (lastRef) releaseGrant(uri)
    }

    // ------------------------------------------------------------- helpers

    private fun buildOpenIntent(config: PickConfig, multi: Boolean): Intent {
        val mimes = config.filter.mimeTypes.ifEmpty { listOf("*/*") }
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
            pending.set(cont)
            // The Rust caller dropped its future: forget the continuation
            // so a late result is ignored. The system picker itself
            // cannot be dismissed programmatically.
            cont.invokeOnCancellation { pending.compareAndSet(cont, null) }
            try {
                launcher.launch(intent)
            } catch (t: Throwable) {
                if (pending.compareAndSet(cont, null)) {
                    cont.resumeWith(
                        Result.failure(BackendException(FilePickerError.Backend(t.message ?: "launch failed"))),
                    )
                }
            }
        }
    }

    private fun resumePending(result: ActivityResult) {
        val cont = pending.getAndSet(null) ?: return
        if (cont.isActive) cont.resume(result)
    }

    /** Read / write grants the provider actually handed out with the result. */
    private fun grantedFlags(data: Intent): Int {
        val flags = data.flags and
            (Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
        return if (flags == 0) Intent.FLAG_GRANT_READ_URI_PERMISSION else flags
    }

    private fun registerAndDescribe(uri: Uri, flags: Int): PickedFile {
        val firstRef = synchronized(uriRefs) {
            val refs = (uriRefs[uri] ?: 0) + 1
            uriRefs[uri] = refs
            refs == 1
        }
        if (firstRef) takeGrant(uri, flags)
        val id = IstmoRuntime.allocHandleId(PLUGIN_ID)
        files[id] = uri
        val (name, size, mime) = describe(uri)
        return PickedFile(displayName = name, mimeType = mime, size = size?.toULong(), handle = id)
    }

    private fun takeGrant(uri: Uri, flags: Int) {
        try {
            resolver.takePersistableUriPermission(uri, flags)
            grantLog.edit().putInt(uri.toString(), flags).apply()
        } catch (_: SecurityException) {
            // Some providers (e.g. transient in-memory ones) do not
            // support persistence; the URI still works for the current
            // process, so keep going.
        }
    }

    private fun releaseGrant(uri: Uri) {
        val flags = grantLog.getInt(uri.toString(), 0)
        grantLog.edit().remove(uri.toString()).apply()
        if (flags == 0) return
        try {
            resolver.releasePersistableUriPermission(uri, flags)
        } catch (_: SecurityException) {
            // Permission may already have been dropped by the system.
        }
    }

    private fun releaseOrphanedGrants() {
        val recorded = grantLog.all.keys.toList()
        if (recorded.isEmpty()) return
        val persisted = resolver.persistedUriPermissions.map { it.uri.toString() }.toSet()
        for (key in recorded) {
            if (key in persisted) {
                releaseGrant(Uri.parse(key))
            } else {
                grantLog.edit().remove(key).apply()
            }
        }
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
        private const val PLUGIN_ID = "istmo.file_picker"
        private const val GRANT_PREFS = "dev.istmo.plugins.filepicker.grants"

        // Handle state is per process, not per backend: the backend is
        // rebuilt with every host activity, while the handles it handed
        // out keep living on the Rust side and must still resolve (and
        // release) through whichever instance is registered now.
        private val files = ConcurrentHashMap<Long, Uri>()

        /** Live handles per URI: picking the same document twice yields two handles, one grant. */
        private val uriRefs = HashMap<Uri, Int>()

        /** Orphaned grants are reconciled once per process, before any handle exists. */
        private val reconciled = AtomicBoolean(false)
    }
}
