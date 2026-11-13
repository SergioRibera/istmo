package dev.istmo.plugins.share

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.ClipData
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.graphics.BitmapFactory
import android.net.Uri
import android.os.Build
import android.webkit.MimeTypeMap
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.FileProvider
import androidx.core.content.pm.ShortcutInfoCompat
import androidx.core.content.pm.ShortcutManagerCompat
import androidx.core.graphics.drawable.IconCompat
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.IstmoActivityBound
import dev.istmo.runtime.ShareBackend
import dev.istmo.runtime.ShareCapabilities
import dev.istmo.runtime.ShareError
import dev.istmo.runtime.ShareFile
import dev.istmo.runtime.ShareFileSource
import dev.istmo.runtime.ShareOutcome
import dev.istmo.runtime.ShareRequest
import dev.istmo.runtime.ShareTarget
import java.io.File
import java.lang.ref.WeakReference
import java.util.UUID
import kotlin.coroutines.resume
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.withContext

/**
 * Reference Android backend for `istmo.share`.
 *
 * Sends through `Intent.createChooser` with `ACTION_SEND` /
 * `ACTION_SEND_MULTIPLE`. Files are exposed as `content://` URIs by
 * [IstmoShareFileProvider] (declared in this plugin's
 * `AndroidManifest.xml`, merged by the istmo Gradle plugin), whose roots
 * are only the `istmo-share/` directories of the app's cache and files
 * dirs; in-memory files and every other path are copied into
 * `cacheDir/istmo-share/outgoing/` first.
 *
 * The chosen target is reported through an `IntentSender` callback
 * ([ShareTargetChosenReceiver]); Android never reports whether the
 * target actually completed, so the best outcome is
 * `ShareOutcome.TargetChosen`. The system chooser cannot be closed
 * programmatically: cancelling the Rust call resolves it, but the sheet
 * stays until the user leaves it.
 *
 * Direct-share targets ([set_share_targets]) are published as
 * long-lived sharing shortcuts in the [DIRECT_SHARE_CATEGORY] category,
 * which `res/xml/istmo_share_shortcuts.xml` binds to the app's
 * `dev.istmo.plugins.share.ShareTarget` activity-alias. Launcher-only
 * targets skip that category: they are plain launcher shortcuts and
 * need no alias. Tapping any of them in the launcher opens its `uri`.
 *
 * The backend outlives its activity ([IstmoActivityBound]): the generated
 * registry builds it once and hands it each new activity, so an open
 * share sheet survives a recreation. Registering it by hand:
 *
 * ```kotlin
 * val backend = ShareBackendImpl(this)
 * IstmoRuntime.registerHandler(ShareDispatcher.PLUGIN_ID, ShareDispatcher(backend, ShareCodecsImpl()))
 * ```
 */
class ShareBackendImpl(
    // Only used to register the chooser launcher; not kept, so a backend
    // outliving its activity does not hold on to it.
    activity: ComponentActivity,
) : ShareBackend, IstmoActivityBound {

    private val context: Context = activity.applicationContext
    private val authority = "${context.packageName}$AUTHORITY_SUFFIX"
    private val outgoingRoot = File(context.cacheDir, "/outgoing")

    /** Serialises share sheets — one chooser at a time. */
    private val sheetMutex = Mutex()
    private var pendingResult: CancellableContinuation<Unit>? = null

    /** Activity whose registry holds [chooserLauncher]; weak, never kept alive. */
    @Volatile
    private var hostRef = WeakReference<ComponentActivity>(null)

    // Declared after [hostRef], which [registerChooser] sets.
    @Volatile
    private var chooserLauncher: ActivityResultLauncher<Intent> = registerChooser(activity)

    /**
     * Move the chooser launcher to [activity]. Registered under a fixed key
     * rather than through the lifecycle-bound `registerForActivityResult`:
     * this may run after [activity] started, and a chooser still open
     * across a recreation reports its result to the same key on the new
     * activity's registry.
     */
    override fun attachActivity(activity: Activity) {
        // The registry only hands over activities of the declared class.
        val host = activity as ComponentActivity
        // Registering the key again on the same registry replaces it, and
        // unregistering the old launcher would then drop the new one.
        if (host === hostRef.get()) return
        val next = registerChooser(host)
        val previous = chooserLauncher
        chooserLauncher = next
        previous.unregister()
    }

    private fun registerChooser(activity: ComponentActivity): ActivityResultLauncher<Intent> {
        hostRef = WeakReference(activity)
        return activity.activityResultRegistry.register(
            CHOOSER_RESULT_KEY,
            ActivityResultContracts.StartActivityForResult(),
        ) {
            val cont = pendingResult ?: return@register
            pendingResult = null
            cont.resume(Unit)
        }
    }

    override suspend fun share(request: ShareRequest): ShareOutcome {
        validate(request)
        if (!sheetMutex.tryLock()) throw BackendException(ShareError.Busy)
        try {
            val uris = withContext(Dispatchers.IO) {
                pruneOutgoing()
                request.files.map { stage(it) }
            }
            val thumbnail = request.preview?.thumbnail?.let { withContext(Dispatchers.IO) { stage(it) } }
            val chooser = buildChooser(request, uris, thumbnail)

            ShareTargetChosenReceiver.reset()
            suspendCancellableCoroutine { cont ->
                pendingResult = cont
                cont.invokeOnCancellation { pendingResult = null }
                try {
                    chooserLauncher.launch(chooser)
                } catch (t: Throwable) {
                    pendingResult = null
                    cont.resumeWith(Result.failure(BackendException(ShareError.Backend(t.message ?: "chooser launch failed"))))
                }
            }
            // The chosen-target broadcast can land just after the chooser
            // result; give it a moment before concluding nothing was picked.
            val chosen = ShareTargetChosenReceiver.awaitChosen(CHOSEN_GRACE_MS)
            return if (chosen != null) ShareOutcome.TargetChosen(chosen) else ShareOutcome.Unknown
        } finally {
            sheetMutex.unlock()
        }
    }

    override suspend fun capabilities(): ShareCapabilities = ShareCapabilities(
        send = true,
        files = true,
        mixedContent = true,
        richPreview = Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q,
        reportsCompletion = false,
        reportsTarget = true,
        receive = ShareReceiverActivity.isReceiveEnabled(context),
        directShare = ShareReceiverActivity.isReceiveEnabled(context),
        dismissOnCancel = false,
    )

    override suspend fun staging_dir(): String = withContext(Dispatchers.IO) {
        if (!outgoingRoot.isDirectory && !outgoingRoot.mkdirs()) {
            throw BackendException(ShareError.Io("cannot create ${outgoingRoot.path}"))
        }
        outgoingRoot.absolutePath
    }

    override suspend fun set_share_targets(targets: List<ShareTarget>) {
        if (targets.any { !it.launcherOnly } && !ShareReceiverActivity.isReceiveEnabled(context)) {
            throw BackendException(
                ShareError.Unsupported("declare the dev.istmo.plugins.share.ShareTarget activity-alias to receive shares"),
            )
        }
        withContext(Dispatchers.IO) {
            val launch = context.packageManager.getLaunchIntentForPackage(context.packageName)
                ?: Intent(Intent.ACTION_MAIN).setPackage(context.packageName)
            val appIcon = IconCompat.createWithResource(context, context.applicationInfo.icon)
            val max = ShortcutManagerCompat.getMaxShortcutCountPerActivity(context)
            val shortcuts = targets.take(max).mapIndexed { rank, target ->
                // The launcher tap opens the target's deep link, else the app.
                val open = target.uri
                    ?.let { Intent(Intent.ACTION_VIEW, Uri.parse(it)).setPackage(context.packageName) }
                    ?: Intent(launch).setAction(Intent.ACTION_MAIN)
                ShortcutInfoCompat.Builder(context, SHORTCUT_PREFIX + target.id)
                    .setShortLabel(target.label)
                    .setRank(rank)
                    .setIntent(open)
                    .setIcon(target.icon?.let(::loadIcon) ?: appIcon)
                    .apply {
                        if (!target.launcherOnly) {
                            setLongLived(true)
                            setCategories(setOf(DIRECT_SHARE_CATEGORY))
                        }
                    }
                    .build()
            }
            // Replace only the plugin's shortcuts; the app's own stay.
            val ours = ShortcutManagerCompat.getDynamicShortcuts(context)
                .map { it.id }
                .filter { it.startsWith(SHORTCUT_PREFIX) }
            ShortcutManagerCompat.removeDynamicShortcuts(context, ours)
            shortcuts.forEach { ShortcutManagerCompat.pushDynamicShortcut(context, it) }
        }
    }

    private fun loadIcon(file: ShareFile): IconCompat? {
        val bitmap = when (val source = file.source) {
            is ShareFileSource.Path -> BitmapFactory.decodeFile(source.value)
            is ShareFileSource.Bytes -> BitmapFactory.decodeByteArray(source.value, 0, source.value.size)
        } ?: return null
        return IconCompat.createWithAdaptiveBitmap(bitmap)
    }

    // ------------------------------------------------------------- intent

    private fun buildChooser(request: ShareRequest, uris: List<StagedUri>, thumbnail: StagedUri?): Intent {
        val send = Intent(if (uris.size > 1) Intent.ACTION_SEND_MULTIPLE else Intent.ACTION_SEND).apply {
            type = commonMimeType(request, uris)
            textWithUrl(request)?.let { putExtra(Intent.EXTRA_TEXT, it) }
            request.subject?.let { putExtra(Intent.EXTRA_SUBJECT, it) }
            when (uris.size) {
                0 -> Unit
                1 -> putExtra(Intent.EXTRA_STREAM, uris[0].uri)
                else -> putParcelableArrayListExtra(Intent.EXTRA_STREAM, ArrayList(uris.map { it.uri }))
            }
            val preview = request.preview
            if (preview != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                (preview.title ?: request.subject)?.let { putExtra(Intent.EXTRA_TITLE, it) }
            }
            val clipUris = uris.map { it.uri } + listOfNotNull(thumbnail?.uri)
            if (clipUris.isNotEmpty()) {
                // ClipData carries the read grant to the chooser and, on
                // Android 10+, the first item doubles as the preview
                // thumbnail.
                val ordered = if (thumbnail != null) listOf(thumbnail.uri) + uris.map { it.uri } else clipUris
                val clip = ClipData.newRawUri(null, ordered.first())
                ordered.drop(1).forEach { clip.addItem(ClipData.Item(it)) }
                clipData = clip
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
        }
        val callback = PendingIntent.getBroadcast(
            context,
            CHOSEN_REQUEST_CODE,
            Intent(context, ShareTargetChosenReceiver::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or
                (if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PendingIntent.FLAG_MUTABLE else 0),
        )
        return Intent.createChooser(send, request.subject, callback.intentSender)
    }

    private fun commonMimeType(request: ShareRequest, uris: List<StagedUri>): String {
        if (uris.isEmpty()) return "text/plain"
        val types = uris.map { it.mimeType }.distinct()
        if (types.size == 1) return types[0]
        val majors = types.map { it.substringBefore('/') }.distinct()
        return if (majors.size == 1) "${majors[0]}/*" else "*/*"
    }

    private fun textWithUrl(request: ShareRequest): String? {
        val text = request.text
        val url = request.url
        return when {
            text != null && url != null && text.contains(url) -> text
            text != null && url != null -> "$text\n$url"
            else -> text ?: url
        }
    }

    // ------------------------------------------------------------- staging

    private data class StagedUri(val uri: Uri, val mimeType: String)

    private fun stage(file: ShareFile): StagedUri {
        val name = file.name ?: (file.source as? ShareFileSource.Path)?.let { File(it.value).name } ?: "shared"
        val mime = file.mimeType ?: guessMime(name)
        val staged = when (val source = file.source) {
            is ShareFileSource.Path -> {
                val src = File(source.value)
                if (!src.isFile) throw BackendException(ShareError.NotFound(source.value))
                if (isProviderRoot(src)) src else copyToOutgoing(name) { out -> src.inputStream().use { it.copyTo(out) } }
            }
            is ShareFileSource.Bytes -> copyToOutgoing(name) { out -> out.write(source.value) }
        }
        val uri = try {
            FileProvider.getUriForFile(context, authority, staged, name)
        } catch (e: IllegalArgumentException) {
            throw BackendException(ShareError.Io("FileProvider cannot expose ${staged.path}: ${e.message}"))
        }
        return StagedUri(uri, mime)
    }

    private fun copyToOutgoing(name: String, write: (java.io.OutputStream) -> Unit): File {
        val dir = File(outgoingRoot, UUID.randomUUID().toString())
        if (!dir.mkdirs()) throw BackendException(ShareError.Io("cannot create ${dir.path}"))
        val dest = File(dir, sanitize(name))
        try {
            dest.outputStream().use(write)
        } catch (e: java.io.IOException) {
            throw BackendException(ShareError.Io("${dest.path}: ${e.message}"))
        }
        return dest
    }

    /**
     * Whether [IstmoShareFileProvider] can expose [file] as is. Its roots
     * (`res/xml/istmo_share_paths.xml`) are only the `istmo-share/`
     * directories of the app's cache and files dirs, so nothing else in the
     * sandbox is ever reachable through it; other files are copied in first.
     */
    private fun isProviderRoot(file: File): Boolean {
        val canonical = file.canonicalPath
        return listOf(context.cacheDir, context.filesDir)
            .map { File(it, SHARE_DIR).canonicalPath + File.separator }
            .any { canonical.startsWith(it) }
    }

    /** Receivers read shared files lazily, so outgoing copies are kept for a day. */
    private fun pruneOutgoing() {
        val cutoff = System.currentTimeMillis() - OUTGOING_TTL_MS
        outgoingRoot.listFiles()?.filter { it.lastModified() < cutoff }?.forEach { it.deleteRecursively() }
    }

    private fun validate(request: ShareRequest) {
        if (request.text == null && request.url == null && request.files.isEmpty()) {
            throw BackendException(ShareError.InvalidRequest("share request has no text, url or files"))
        }
    }

    companion object {
        /** Must match `android:authorities` in the plugin manifest. */
        const val AUTHORITY_SUFFIX = ".istmo.share.files"

        /** Must match `<share-target><category>` in `istmo_share_shortcuts.xml`. */
        const val DIRECT_SHARE_CATEGORY = "dev.istmo.share.category.DIRECT_SHARE"

        /** Prefixes the plugin's shortcut ids so the app's own shortcuts are left alone. */
        const val SHORTCUT_PREFIX = "istmo.share/"

        private const val CHOSEN_REQUEST_CODE = 0x15_70_5E
        private const val CHOSEN_GRACE_MS = 500L
        private const val CHOOSER_RESULT_KEY = "dev.istmo.share.chooser"
        private const val OUTGOING_TTL_MS = 24L * 60 * 60 * 1000

        internal fun guessMime(name: String): String {
            val ext = name.substringAfterLast('.', "").lowercase()
            return MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext) ?: "application/octet-stream"
        }

        /** Provider-visible directory under `cacheDir` and `filesDir`. */
        internal const val SHARE_DIR = "istmo-share"

        private const val FORBIDDEN_NAME_CHARS = "<>:\"|?*"

        /**
         * File name that cannot escape the directory it is joined to.
         *
         * Port of `sanitize_file_name` in `plugins/share/src/lib.rs`; keep
         * the rules identical and check them against its `sanitize_vectors`
         * test:
         * 1. split on `/` and `\\` and keep the last non-empty component;
         * 2. replace `< > : " | ? *` and C0 / C1 controls (U+0000–U+001F,
         *    U+007F–U+009F) with `_`;
         * 3. a result made only of dots (or empty) becomes `shared`;
         * 4. a Windows device name ([WINDOWS_DEVICE_NAMES], ASCII case
         *    insensitive) before the first `.`, trailing spaces ignored,
         *    gets a `_` prefix;
         * 5. past [MAX_FILE_NAME_BYTES] UTF-8 bytes the name is cut at a
         *    code point boundary, keeping an extension (from the last `.`,
         *    not at index 0) of up to [MAX_KEPT_EXTENSION_BYTES] bytes.
         */
        internal fun sanitize(name: String): String {
            val base = name.split('/', '\\').lastOrNull { it.isNotEmpty() }.orEmpty()
            val cleaned = buildString(base.length) {
                for (c in base) {
                    val control = c.code <= 0x1F || c.code in 0x7F..0x9F
                    append(if (control || c in FORBIDDEN_NAME_CHARS) '_' else c)
                }
            }
            var result = if (cleaned.all { it == '.' }) "shared" else cleaned
            val stem = result.substringBefore('.').trimEnd(' ')
            val folded = asciiFold(stem)
            if (WINDOWS_DEVICE_NAMES.any { it == folded }) {
                result = "_$result"
            }
            return truncate(result)
        }

        private const val MAX_FILE_NAME_BYTES = 255
        private const val MAX_KEPT_EXTENSION_BYTES = 16

        private val WINDOWS_DEVICE_NAMES = listOf(
            "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$",
            "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
            "COM¹", "COM²", "COM³",
            "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
            "LPT¹", "LPT²", "LPT³",
        )

        /** Upper-case ASCII letters only, like Rust's `eq_ignore_ascii_case`. */
        private fun asciiFold(s: String): String =
            String(CharArray(s.length) { i -> s[i].let { if (it in 'a'..'z') it - 32 else it } })

        private fun utf8Length(codePoint: Int): Int = when {
            codePoint < 0x80 -> 1
            codePoint < 0x800 -> 2
            codePoint < 0x10000 -> 3
            else -> 4
        }

        private fun utf8Length(s: String): Int {
            var total = 0
            var i = 0
            while (i < s.length) {
                val cp = s.codePointAt(i)
                total += utf8Length(cp)
                i += Character.charCount(cp)
            }
            return total
        }

        private fun truncate(name: String): String {
            val total = utf8Length(name)
            if (total <= MAX_FILE_NAME_BYTES) return name
            val dot = name.lastIndexOf('.')
            val ext = if (dot > 0 && utf8Length(name.substring(dot)) <= MAX_KEPT_EXTENSION_BYTES) {
                name.substring(dot)
            } else {
                ""
            }
            val stem = name.substring(0, name.length - ext.length)
            val budget = MAX_FILE_NAME_BYTES - utf8Length(ext)
            var used = 0
            var end = 0
            while (end < stem.length) {
                val cp = stem.codePointAt(end)
                if (used + utf8Length(cp) > budget) break
                used += utf8Length(cp)
                end += Character.charCount(cp)
            }
            return stem.substring(0, end) + ext
        }
    }
}

/**
 * Receives the `IntentSender` callback `Intent.createChooser` fires when
 * the user picks a target. Declared (not exported) in the plugin
 * manifest.
 */
class ShareTargetChosenReceiver : BroadcastReceiver() {

    override fun onReceive(context: Context, intent: Intent) {
        val component: ComponentName? = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            intent.getParcelableExtra(Intent.EXTRA_CHOSEN_COMPONENT, ComponentName::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableExtra(Intent.EXTRA_CHOSEN_COMPONENT)
        }
        chosen = component?.flattenToShortString()
    }

    companion object {
        @Volatile
        private var chosen: String? = null

        internal fun reset() {
            chosen = null
        }

        internal suspend fun awaitChosen(graceMs: Long): String? {
            val step = 50L
            var waited = 0L
            while (chosen == null && waited < graceMs) {
                delay(step)
                waited += step
            }
            return chosen
        }
    }
}

/**
 * `FileProvider` subclass so the plugin's provider never collides with
 * one the app declares itself. Paths live in `res/xml/istmo_share_paths.xml`.
 */
class IstmoShareFileProvider : FileProvider()
