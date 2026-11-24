package dev.istmo.plugins.webview

import android.app.DownloadManager
import android.content.Context
import android.net.Uri
import android.os.Message
import android.print.PrintAttributes
import android.print.PrintManager
import android.webkit.CookieManager
import android.webkit.WebSettings
import android.webkit.WebStorage
import android.webkit.WebView
import androidx.activity.ComponentActivity
import androidx.webkit.ProfileStore
import androidx.webkit.WebViewFeature
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.HandleReleaser
import dev.istmo.runtime.IstmoRuntime
import java.io.File
import java.security.SecureRandom
import java.util.concurrent.ConcurrentHashMap
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext

/**
 * `istmo.webview` on Android `WebView` + androidx.webkit.
 *
 * Embedded views are `PopupWindow`s placed in physical pixels; windowed
 * views are full-screen dialogs. Every engine call runs on the main
 * thread; scheme bodies stream from the IO dispatcher.
 */
class WebViewBackendImpl(
    activity: ComponentActivity,
    internal val config: WebViewInitConfig,
) : WebViewBackend, HandleReleaser {
    @Volatile
    private var activity: ComponentActivity = activity
    private val views = ConcurrentHashMap<Long, ManagedView>()
    private val zoom = ConcurrentHashMap<Long, Float>()
    private val downloads = ConcurrentHashMap<ULong, Long>()
    private val random = SecureRandom()
    internal val schemes = SchemeServer(activity.applicationContext.assets, config.schemes)

    private val multiProfile: Boolean
        get() = WebViewFeature.isFeatureSupported(WebViewFeature.MULTI_PROFILE)

    fun attachActivity(next: ComponentActivity) {
        activity = next
        views.values.forEach { it.reattach(next) }
    }

    private fun view(ref: ViewRef): ManagedView {
        val view = views[ref.view] ?: throw BackendException(WebViewError.ViewClosed)
        // Constant-time token comparison.
        val diff = (view.token.first xor ref.tokenHi) or (view.token.second xor ref.tokenLo)
        if (diff != 0UL) throw BackendException(WebViewError.InvalidToken)
        return view
    }

    private suspend fun <T> onMain(block: () -> T): T = withContext(Dispatchers.Main) { block() }

    override suspend fun capabilities(): WebViewCapabilities = WebViewCapabilities(
        backend = "android-webview",
        engineVersion = androidx.webkit.WebViewCompat.getCurrentWebViewPackage(activity)?.versionName ?: "",
        embedded = true,
        windowed = true,
        windowParenting = false,
        persistentProfiles = multiProfile,
        ephemeralProfiles = multiProfile,
        isolatedWorld = false,
        bridgeReply = WebViewFeature.isFeatureSupported(WebViewFeature.WEB_MESSAGE_LISTENER),
        deferredNavigation = false,
        schemeRequestBodies = false,
        downloads = true,
        downloadControl = false,
        find = true,
        print = true,
        snapshot = true,
        inspector = config.allowInspector,
        schemeOrigins = config.schemes.map {
            SchemeOrigin(it.name, SchemeServer.ORIGIN, "${SchemeServer.ORIGIN}/${it.name}/")
        },
    )

    override suspend fun prewarm() = onMain {
        // Loads the WebView provider (the slow part of the first view).
        WebSettings.getDefaultUserAgent(activity)
        Unit
    }

    override suspend fun open(spec: WebViewSpec): WebViewInstance = onMain {
        val (profile, ephemeral) = when (val p = spec.profile) {
            is WebProfile.Shared -> null to false
            is WebProfile.Persistent -> {
                if (!multiProfile) throw BackendException(WebViewError.ProfileUnsupported)
                if (!p.value.name.matches(Regex("[A-Za-z0-9._-]{1,64}"))) {
                    throw BackendException(WebViewError.InvalidArgument("profile name"))
                }
                "istmo.${p.value.name}" to false
            }
            // Android has no in-memory profile: a fresh named one, deleted
            // when the view closes. Without profiles it would share the
            // process cookie jar, so it is refused.
            is WebProfile.Ephemeral -> {
                if (!multiProfile) throw BackendException(WebViewError.ProfileUnsupported)
                "istmo.ephemeral.${random.nextLong().toULong().toString(16)}" to true
            }
        }
        if (profile != null) ProfileStore.getInstance().getOrCreateProfile(profile)
        create(spec, profile, ephemeral, null)
    }

    private fun create(spec: WebViewSpec, profile: String?, ephemeral: Boolean, reuse: WebView?): WebViewInstance {
        val id = IstmoRuntime.allocHandleId(WEBVIEW_ID)
        val token = random.nextLong().toULong() to random.nextLong().toULong()
        val view = try {
            ManagedView(this, activity, id, token, spec, profile, ephemeral, reuse)
        } catch (e: IllegalArgumentException) {
            IstmoRuntime.forgetHandle(id)
            throw BackendException(WebViewError.InvalidArgument(e.message ?: "bad spec"))
        }
        views[id] = view
        return WebViewInstance(id, token.first, token.second)
    }

    /** `window.open` the app allowed: a new view with the opener's config. */
    internal fun openPopup(opener: ManagedView, transport: Message, url: String) {
        val spec = opener.spec.copy(target = RenderTarget.Window(WindowTarget("", 0U, 0U, null, false, true)))
        val instance = create(spec, opener.profileName, false, null)
        val child = views[instance.view] ?: return
        (transport.obj as WebView.WebViewTransport).webView = child.webView
        transport.sendToTarget()
        opener.emit(ControlEvent.NewView(NewViewInfo(instance, url)))
    }

    internal fun cookieManager(profile: String?): CookieManager =
        profile?.let { ProfileStore.getInstance().getProfile(it)?.cookieManager } ?: CookieManager.getInstance()

    private fun webStorage(profile: String?): WebStorage =
        profile?.let { ProfileStore.getInstance().getProfile(it)?.webStorage } ?: WebStorage.getInstance()

    internal fun deleteProfile(name: String) {
        // Deletion fails while a WebView still uses it; try after the
        // view's teardown has run.
        activity.window?.decorView?.post {
            try {
                ProfileStore.getInstance().deleteProfile(name)
            } catch (_: IllegalStateException) {
            }
        }
    }

    internal fun closeAfterCrash(view: ManagedView) {
        activity.window?.decorView?.post { remove(view.id) }
    }

    private fun remove(id: Long) {
        views.remove(id)?.close()
        zoom.remove(id)
        IstmoRuntime.forgetHandle(id)
    }

    internal fun download(view: ManagedView, requestId: ULong, url: String, mime: String, name: String, path: String) {
        val manager = activity.getSystemService(Context.DOWNLOAD_SERVICE) as DownloadManager
        val target = File(path).let { if (it.isDirectory) File(it, name) else it }
        val request = DownloadManager.Request(Uri.parse(url))
            .setMimeType(mime)
            .setDestinationUri(Uri.fromFile(target))
            .setNotificationVisibility(DownloadManager.Request.VISIBILITY_VISIBLE_NOTIFY_COMPLETED)
        cookieManager(view.profileName).getCookie(url)?.let { request.addRequestHeader("Cookie", it) }
        downloads[requestId] = manager.enqueue(request)
    }

    override suspend fun close(view: ViewRef) = onMain {
        view(view)
        remove(view.view)
    }

    override fun releaseNativeHandle(handleId: Long) {
        if (!views.containsKey(handleId)) return
        activity.runOnUiThread { remove(handleId) }
    }

    override suspend fun set_frame(view: ViewRef, frame: ViewFrame) = onMain { view(view).setFrame(frame) }

    override suspend fun focus(view: ViewRef) = onMain {
        view(view).webView.requestFocus()
        Unit
    }

    override suspend fun load(view: ViewRef, request: LoadRequest) = onMain { view(view).load(request) }

    override suspend fun load_html(view: ViewRef, html: String, base_url: String?) =
        onMain { view(view).loadHtml(html, base_url) }

    override suspend fun navigate_history(view: ViewRef, nav: HistoryNav) = onMain {
        val web = view(view).webView
        when (nav) {
            is HistoryNav.Back -> web.goBack()
            is HistoryNav.Forward -> web.goForward()
            is HistoryNav.Reload -> web.reload()
            is HistoryNav.ReloadBypassCache -> {
                web.clearCache(false)
                web.reload()
            }
            is HistoryNav.Stop -> web.stopLoading()
            is HistoryNav.Offset -> if (web.canGoBackOrForward(nav.value)) web.goBackOrForward(nav.value)
        }
    }

    override suspend fun history(view: ViewRef): WebHistory = onMain { view(view).history() }

    override suspend fun eval(view: ViewRef, call: ScriptCall): String = withContext(Dispatchers.Main) {
        val target = view(view)
        suspendCancellableCoroutine { cont ->
            target.eval(call) { result ->
                result.fold({ cont.resume(it) }, { cont.resumeWithException(it) })
            }
        }
    }

    override suspend fun post_message(view: ViewRef, message: OutgoingMessage) =
        onMain { view(view).postMessage(message) }

    override suspend fun reply(view: ViewRef, reply: BridgeReply) = onMain { view(view).reply(reply) }

    override suspend fun decide(view: ViewRef, decision: Decision) = onMain { view(view).decide(decision) }

    override suspend fun scheme_head(view: ViewRef, head: SchemeResponseHead) {
        schemes.head(view(view), head)
    }

    override suspend fun scheme_chunk(view: ViewRef, chunk: SchemeChunk) {
        val target = view(view)
        // May wait for the page to read: never on the main thread.
        withContext(Dispatchers.IO) { schemes.chunk(target, chunk) }
    }

    override fun scheme_request_body(view: ViewRef, request_id: ULong): Flow<ByteArray> = flow {
        throw BackendException(WebViewError.Unsupported("Android WebView does not expose request bodies"))
    }

    override suspend fun cookies(view: ViewRef, filter: CookieFilter): List<WebCookie> = onMain {
        val target = view(view)
        val url = filter.url ?: throw BackendException(WebViewError.Unsupported("listing cookies needs a URL on Android"))
        val host = Uri.parse(url).host ?: ""
        val raw = cookieManager(target.profileName).getCookie(url) ?: ""
        raw.split(';').mapNotNull { pair ->
            val name = pair.substringBefore('=').trim()
            if (name.isEmpty() || (filter.name != null && filter.name != name)) return@mapNotNull null
            // `getCookie` returns name=value pairs only.
            WebCookie(name, pair.substringAfter('=', "").trim(), host, "/", null, false, false, SameSite.Lax)
        }
    }

    override suspend fun set_cookie(view: ViewRef, cookie: WebCookie) = withContext(Dispatchers.Main) {
        val manager = cookieManager(view(view).profileName)
        val header = buildString {
            append("${cookie.name}=${cookie.value}; Domain=${cookie.domain}; Path=${cookie.path.ifEmpty { "/" }}")
            cookie.expiresUnixMs?.let { append("; Max-Age=${((it - System.currentTimeMillis()) / 1000).coerceAtLeast(0)}") }
            if (cookie.secure) append("; Secure")
            if (cookie.httpOnly) append("; HttpOnly")
            append("; SameSite=${cookie.sameSite.name}")
        }
        val scheme = if (cookie.secure) "https" else "http"
        suspendCancellableCoroutine<Unit> { cont ->
            manager.setCookie("$scheme://${cookie.domain.trimStart('.')}${cookie.path.ifEmpty { "/" }}", header) { ok ->
                if (ok) cont.resume(Unit) else cont.resumeWithException(BackendException(WebViewError.InvalidArgument("cookie rejected")))
            }
        }
    }

    override suspend fun clear_data(view: ViewRef, request: ClearDataRequest) = onMain {
        val target = view(view)
        val all = DataKind.All in request.kinds
        if (all || DataKind.Cookies in request.kinds) cookieManager(target.profileName).removeAllCookies(null)
        if (all || DataKind.Cache in request.kinds) target.webView.clearCache(true)
        if (all || DataKind.LocalStorage in request.kinds || DataKind.IndexedDb in request.kinds) {
            webStorage(target.profileName).deleteAllData()
        }
    }

    override suspend fun set_zoom(view: ViewRef, factor: Float) = onMain {
        val target = view(view)
        val current = zoom[target.id] ?: 1f
        val step = (factor / current).coerceIn(0.01f, 100f)
        target.webView.zoomBy(step)
        zoom[target.id] = factor
    }

    override suspend fun find(view: ViewRef, query: FindQuery): FindResult = withContext(Dispatchers.Main) {
        val target = view(view)
        suspendCancellableCoroutine { cont -> target.find(query) { cont.resume(it) } }
    }

    override suspend fun snapshot(view: ViewRef, options: SnapshotOptions): WebSnapshot =
        onMain { view(view).snapshot(options) }

    override suspend fun print(view: ViewRef) = onMain {
        val target = view(view)
        val manager = activity.getSystemService(Context.PRINT_SERVICE) as PrintManager
        manager.print("web page", target.webView.createPrintDocumentAdapter("web page"), PrintAttributes.Builder().build())
        Unit
    }

    override suspend fun set_inspector(view: ViewRef, open: Boolean) = onMain {
        view(view)
        if (!config.allowInspector) throw BackendException(WebViewError.Unsupported("inspector disabled"))
        // Process-wide on Android.
        WebView.setWebContentsDebuggingEnabled(open)
    }

    override suspend fun download_control(view: ViewRef, op: DownloadOp) = onMain {
        view(view)
        if (op.action != DownloadAction.Cancel) throw BackendException(WebViewError.Unsupported("pausing downloads"))
        val id = downloads.remove(op.downloadId) ?: throw BackendException(WebViewError.UnknownRequest)
        (activity.getSystemService(Context.DOWNLOAD_SERVICE) as DownloadManager).remove(id)
        Unit
    }

    override fun control_events(view: ViewRef): Flow<ControlEvent> = try {
        view(view).controlFlow()
    } catch (e: BackendException) {
        flow { throw e }
    }

    override fun telemetry(view: ViewRef, kinds: List<TelemetryKind>): Flow<TelemetryEvent> = try {
        view(view).telemetryFlow(kinds)
    } catch (e: BackendException) {
        flow { throw e }
    }

    private companion object {
        const val WEBVIEW_ID = "istmo.webview"
    }
}
