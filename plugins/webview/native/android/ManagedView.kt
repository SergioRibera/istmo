package dev.istmo.plugins.webview

import android.annotation.SuppressLint
import android.app.Dialog
import android.content.ActivityNotFoundException
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.net.Uri
import android.net.http.SslError
import android.os.Handler
import android.os.Looper
import android.os.Message
import android.view.Choreographer
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.webkit.ConsoleMessage
import android.webkit.GeolocationPermissions
import android.webkit.HttpAuthHandler
import android.webkit.JsPromptResult
import android.webkit.JsResult
import android.webkit.PermissionRequest
import android.webkit.RenderProcessGoneDetail
import android.webkit.SslErrorHandler
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.FrameLayout
import android.widget.PopupWindow
import androidx.activity.ComponentActivity
import androidx.webkit.JavaScriptReplyProxy
import androidx.webkit.ScriptHandler
import androidx.webkit.WebMessageCompat
import androidx.webkit.WebSettingsCompat
import androidx.webkit.WebViewCompat
import androidx.webkit.WebViewFeature
import dev.istmo.runtime.BackendException
import java.io.ByteArrayOutputStream
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.consumeAsFlow
import kotlinx.coroutines.flow.flow
import org.json.JSONObject

/**
 * One web view and everything attached to it. Every method runs on the
 * main thread except where noted (`shouldInterceptRequest` and the
 * scheme pump run on WebView's network threads).
 */
@SuppressLint("SetJavaScriptEnabled")
internal class ManagedView(
    private val backend: WebViewBackendImpl,
    activity: ComponentActivity,
    val id: Long,
    val token: Pair<ULong, ULong>,
    val spec: WebViewSpec,
    val profileName: String?,
    private val deleteProfileOnClose: Boolean,
    reuse: WebView? = null,
) {
    val webView: WebView = reuse ?: WebView(activity)
    private val rules = CompiledRules(spec.rules)
    private val main = Handler(Looper.getMainLooper())
    private val random = SecureRandom()

    // ---- presentation ---------------------------------------------------
    private var popup: PopupWindow? = null
    private var dialog: Dialog? = null
    private var activity: ComponentActivity = activity
    private var pendingFrame: ViewFrame? = null
    private var frameScheduled = false
    private var appliedFrame: ViewFrame? = null

    // ---- lanes ----------------------------------------------------------
    /** Lossless; buffers until the first (single) collector. */
    private val control = Channel<ControlEvent>(Channel.UNLIMITED)
    @Volatile
    private var controlTaken = false
    private val telemetrySubs = ConcurrentHashMap<Channel<TelemetryEvent>, Set<TelemetryKind>>()
    private val telemetryDropped = ConcurrentHashMap<Channel<TelemetryEvent>, Int>()

    // ---- navigation state ----------------------------------------------
    private var navigationId = 0UL
    private var documentId = 0UL
    @Volatile
    var committedOrigin: String? = null
        private set
    private var lastProgress = -1

    // ---- pending requests (single-use random ids) -----------------------
    private sealed class Ask {
        class Navigation(val url: String) : Ask()
        class NewWindow(val transport: Message, val url: String) : Ask()
        class Ssl(val handler: SslErrorHandler) : Ask()
        class Auth(val handler: HttpAuthHandler) : Ask()
        class Permission(val request: PermissionRequest) : Ask()
        class Geolocation(val callback: GeolocationPermissions.Callback, val origin: String) : Ask()
        class Files(val callback: ValueCallback<Array<Uri>>) : Ask()
        class Dialog(val result: JsResult) : Ask()
        class Download(val id: ULong, val url: String, val mime: String, val name: String) : Ask()
    }

    private val asks = HashMap<ULong, Ask>()
    private val schemes = ConcurrentHashMap<ULong, SchemeServer.Pending>()
    private val replies = HashMap<ULong, Pair<JavaScriptReplyProxy, Long>>()
    private val proxyDocuments = HashMap<JavaScriptReplyProxy, ULong>()
    private var mainProxy: JavaScriptReplyProxy? = null
    private val scriptHandlers = ArrayList<ScriptHandler>()

    @Volatile
    var closed = false
        private set

    init {
        configure()
        attach()
    }

    // ===================================================================
    // Setup
    // ===================================================================

    private fun configure() {
        // Must precede every other call on the view.
        if (profileName != null) WebViewCompat.setProfile(webView, profileName)
        val s = spec.settings
        val settings = webView.settings
        settings.javaScriptEnabled = s.javascript
        settings.domStorageEnabled = s.domStorage
        settings.mediaPlaybackRequiresUserGesture = !s.mediaAutoplay
        settings.builtInZoomControls = s.zoomGestures
        settings.displayZoomControls = false
        settings.setSupportZoom(s.zoomGestures)
        settings.allowFileAccess = s.fileAccess
        settings.allowContentAccess = false
        settings.mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
        settings.setSupportMultipleWindows(true)
        settings.javaScriptCanOpenWindowsAutomatically = false
        settings.setGeolocationEnabled(true)
        val defaultUa = settings.userAgentString
        settings.userAgentString = s.userAgent
            ?: backend.config.userAgentSuffix?.let { "$defaultUa $it" }
            ?: defaultUa
        if (WebViewFeature.isFeatureSupported(WebViewFeature.SAFE_BROWSING_ENABLE)) {
            WebSettingsCompat.setSafeBrowsingEnabled(settings, true)
        }
        if (WebViewFeature.isFeatureSupported(WebViewFeature.WEB_AUTHENTICATION)) {
            WebSettingsCompat.setWebAuthenticationSupport(
                settings,
                if (s.webauthn) WebSettingsCompat.WEB_AUTHENTICATION_SUPPORT_FOR_APP
                else WebSettingsCompat.WEB_AUTHENTICATION_SUPPORT_NONE,
            )
        }
        if (!s.autofill) {
            webView.importantForAutofill = View.IMPORTANT_FOR_AUTOFILL_NO_EXCLUDE_DESCENDANTS
        }
        backend.cookieManager(profileName).setAcceptThirdPartyCookies(webView, s.thirdPartyCookies)
        val bg = s.background
        webView.setBackgroundColor(
            if (s.transparent) Color.TRANSPARENT
            else Color.argb(bg.a.toInt(), bg.r.toInt(), bg.g.toInt(), bg.b.toInt()),
        )
        if (s.contextMenu == ContextMenuMode.Hidden) {
            webView.isLongClickable = false
            webView.setOnLongClickListener { true }
        }
        webView.webViewClient = Client()
        webView.webChromeClient = Chrome()
        webView.setDownloadListener { url, _, contentDisposition, mime, length ->
            onDownload(url, contentDisposition, mime, length)
        }
        installScripts()
        installBridge()
    }

    private fun installScripts() {
        if (!WebViewFeature.isFeatureSupported(WebViewFeature.DOCUMENT_START_SCRIPT)) return
        for (script in spec.userScripts) {
            if (script.injectAt != InjectionTime.DocumentStart || script.world != ScriptWorld.Page) continue
            val origins = script.allowedOrigins.ifEmpty { listOf("*") }.toSet()
            val source = if (script.mainFrameOnly) "if (window === window.top) {\n${script.source}\n}" else script.source
            scriptHandlers += WebViewCompat.addDocumentStartJavaScript(webView, source, origins)
        }
    }

    private fun installBridge() {
        val bridge = spec.bridge ?: return
        if (!WebViewFeature.isFeatureSupported(WebViewFeature.WEB_MESSAGE_LISTENER) ||
            !WebViewFeature.isFeatureSupported(WebViewFeature.DOCUMENT_START_SCRIPT) ||
            bridge.allowedOrigins.isEmpty()
        ) {
            return
        }
        val origins = bridge.allowedOrigins.toSet()
        WebViewCompat.addWebMessageListener(webView, Bridge.NATIVE_OBJECT, origins) { _, message, sourceOrigin, isMainFrame, proxy ->
            onBridgeMessage(message, sourceOrigin.toString(), isMainFrame, proxy)
        }
        scriptHandlers += WebViewCompat.addDocumentStartJavaScript(
            webView,
            Bridge.shim(bridge.allowSubframes),
            origins,
        )
    }

    // ===================================================================
    // Presentation
    // ===================================================================

    private fun attach() {
        when (val target = spec.target) {
            is RenderTarget.Embedded -> {
                val window = PopupWindow(webView, 1, 1, true)
                window.inputMethodMode = PopupWindow.INPUT_METHOD_NEEDED
                window.isClippingEnabled = false
                // A NativeActivity's surface draws over content-view
                // children: a popup window is the layer above it.
                popup = window
                appliedFrame?.let { applyFrame(it) }
            }
            is RenderTarget.Window -> {
                val d = Dialog(activity, android.R.style.Theme_DeviceDefault_NoActionBar)
                (webView.parent as? ViewGroup)?.removeView(webView)
                d.setContentView(
                    FrameLayout(activity).apply {
                        addView(webView, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
                    },
                )
                d.setTitle(target.value.title)
                d.setOnKeyListener { _, keyCode, event ->
                    if (keyCode == android.view.KeyEvent.KEYCODE_BACK &&
                        event.action == android.view.KeyEvent.ACTION_UP && webView.canGoBack()
                    ) {
                        webView.goBack()
                        true
                    } else {
                        false
                    }
                }
                d.setOnCancelListener { emit(ControlEvent.CloseRequested) }
                dialog = d
                d.show()
            }
        }
    }

    /** Move to a recreated activity, keeping the page. */
    fun reattach(next: ComponentActivity) {
        popup?.dismiss()
        dialog?.setOnCancelListener(null)
        dialog?.dismiss()
        (webView.parent as? ViewGroup)?.removeView(webView)
        popup = null
        dialog = null
        activity = next
        attach()
    }

    /** Store the latest frame and apply it on the next display frame. */
    fun setFrame(frame: ViewFrame) {
        pendingFrame = frame
        if (frameScheduled) return
        frameScheduled = true
        Choreographer.getInstance().postFrameCallback {
            frameScheduled = false
            pendingFrame?.let { applyFrame(it) }
            pendingFrame = null
        }
    }

    private fun applyFrame(frame: ViewFrame) {
        appliedFrame = frame
        val window = popup ?: return
        val width = frame.width.toInt().coerceAtLeast(1)
        val height = frame.height.toInt().coerceAtLeast(1)
        when {
            !frame.visible || frame.width == 0U || frame.height == 0U -> if (window.isShowing) window.dismiss()
            window.isShowing -> window.update(frame.x, frame.y, width, height)
            else -> {
                val anchor = activity.window?.decorView ?: return
                window.width = width
                window.height = height
                window.showAtLocation(anchor, Gravity.NO_GRAVITY, frame.x, frame.y)
            }
        }
    }

    // ===================================================================
    // Lanes
    // ===================================================================

    fun emit(event: ControlEvent) {
        if (!closed) control.trySend(event)
    }

    fun controlFlow(): Flow<ControlEvent> {
        if (controlTaken) {
            return flow { throw BackendException(WebViewError.InvalidArgument("control events already have a subscriber")) }
        }
        controlTaken = true
        return control.consumeAsFlow()
    }

    fun telemetryFlow(kinds: List<TelemetryKind>): Flow<TelemetryEvent> {
        val missing = kinds.filter { it !in spec.telemetry }
        if (missing.isNotEmpty()) {
            return flow { throw BackendException(WebViewError.InvalidArgument("telemetry kinds not in the view spec: $missing")) }
        }
        val wanted = kinds.toSet()
        val channel = Channel<TelemetryEvent>(TELEMETRY_DEPTH, BufferOverflow.SUSPEND)
        telemetrySubs[channel] = wanted
        return flow {
            try {
                for (event in channel) emit(event)
            } finally {
                telemetrySubs.remove(channel)
                telemetryDropped.remove(channel)
            }
        }
    }

    private fun wants(kind: TelemetryKind): Boolean = telemetrySubs.values.any { kind in it }

    private fun telemetry(kind: TelemetryKind, event: TelemetryEvent) {
        for ((channel, kinds) in telemetrySubs) {
            if (kind !in kinds) continue
            val dropped = telemetryDropped[channel] ?: 0
            if (dropped > 0) {
                if (channel.trySend(TelemetryEvent.Dropped(dropped.toUInt())).isSuccess) {
                    telemetryDropped.remove(channel)
                } else {
                    telemetryDropped[channel] = dropped + 1
                    continue
                }
            }
            if (channel.trySend(event).isFailure) {
                telemetryDropped[channel] = (telemetryDropped[channel] ?: 0) + 1
            }
        }
    }

    // ===================================================================
    // Navigation
    // ===================================================================

    private fun nextNavigation(): ULong = ++navigationId

    private fun requestId(): ULong {
        var id = random.nextLong().toULong()
        while (id == 0UL || asks.containsKey(id) || schemes.containsKey(id) || replies.containsKey(id)) {
            id = random.nextLong().toULong()
        }
        return id
    }

    /** Run `url` through the rules; `true` when the engine may proceed. */
    private fun admit(url: String, userGesture: Boolean): Boolean {
        val info = NavInfo(nextNavigation(), url, false, userGesture)
        return when (rules.evaluate(url, spec.settings.fileAccess)) {
            RuleAction.Allow -> true
            RuleAction.Deny -> {
                emit(ControlEvent.NavigationFailed(NavFailure(info.navigationId, url, NavErrorKind.Cancelled, "blocked by navigation rules")))
                false
            }
            RuleAction.Intercept -> {
                emit(ControlEvent.Intercepted(InterceptedNav(info.navigationId, url)))
                false
            }
            RuleAction.External -> {
                openExternally(url)
                false
            }
            RuleAction.Ask -> {
                // `shouldOverrideUrlLoading` cannot wait: cancel now and
                // re-issue the load when the app allows it.
                ask(Ask.Navigation(url), DecisionKind.Navigation(info), spec.timeouts.navigationMs.toLong())
                false
            }
        }
    }

    fun load(request: LoadRequest) {
        if (!admit(request.url, false)) return
        val body = request.postBody
        if (body != null) {
            webView.postUrl(request.url, body)
        } else {
            webView.loadUrl(request.url, request.headers.associate { it.name to it.value })
        }
    }

    fun loadHtml(html: String, baseUrl: String?) {
        val bridge = spec.bridge
        if (bridge != null && baseUrl != null) {
            val origin = CompiledRules.originOf(baseUrl)
            if (origin == null || origin !in bridge.allowedOrigins) {
                throw BackendException(WebViewError.InvalidArgument("base_url origin must be in the bridge's allowed_origins"))
            }
        }
        webView.loadDataWithBaseURL(baseUrl, html, "text/html", "utf-8", null)
    }

    private fun openExternally(url: String) {
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(url)).addCategory(Intent.CATEGORY_BROWSABLE)
        try {
            activity.startActivity(intent)
        } catch (_: ActivityNotFoundException) {
            emit(ControlEvent.NavigationFailed(NavFailure(nextNavigation(), url, NavErrorKind.Other, "no app opens this link")))
        }
    }

    fun history(): WebHistory {
        val list = webView.copyBackForwardList()
        val entries = (0 until list.size).map { HistoryEntry(list.getItemAtIndex(it).url ?: "", list.getItemAtIndex(it).title ?: "") }
        return WebHistory(entries, list.currentIndex.coerceAtLeast(0).toUInt())
    }

    // ===================================================================
    // Decisions
    // ===================================================================

    private fun ask(ask: Ask, kind: DecisionKind, timeoutMs: Long?) {
        val id = requestId()
        asks[id] = ask
        emit(ControlEvent.DecisionRequested(DecisionRequest(id, kind)))
        if (timeoutMs != null && timeoutMs > 0) {
            main.postDelayed({ asks.remove(id)?.let { resolve(it, DecisionResponse.Deny) } }, timeoutMs)
        }
    }

    fun decide(decision: Decision) {
        val ask = asks.remove(decision.requestId)
            ?: throw BackendException(WebViewError.UnknownRequest)
        resolve(ask, decision.response)
    }

    private fun resolve(ask: Ask, response: DecisionResponse) {
        val allow = response !is DecisionResponse.Deny
        when (ask) {
            is Ask.Navigation -> if (allow) webView.loadUrl(ask.url)
            is Ask.NewWindow -> if (allow) backend.openPopup(this, ask.transport, ask.url) else ask.transport.recycle()
            is Ask.Ssl -> if (response is DecisionResponse.Allow) ask.handler.proceed() else ask.handler.cancel()
            is Ask.Auth -> if (response is DecisionResponse.Credentials) {
                ask.handler.proceed(response.value.user, response.value.password)
            } else {
                ask.handler.cancel()
            }
            is Ask.Permission -> {
                val granted = when (response) {
                    is DecisionResponse.Allow -> ask.request.resources.toList()
                    is DecisionResponse.Grant -> ask.request.resources.filter { webPermission(it) in response.value }
                    else -> emptyList()
                }
                if (granted.isEmpty()) ask.request.deny() else ask.request.grant(granted.toTypedArray())
            }
            is Ask.Geolocation -> ask.callback.invoke(
                ask.origin,
                response is DecisionResponse.Allow ||
                    (response is DecisionResponse.Grant && WebPermission.Geolocation in response.value),
                false,
            )
            is Ask.Files -> ask.callback.onReceiveValue(
                (response as? DecisionResponse.Files)?.value?.map { path ->
                    if (path.contains("://")) Uri.parse(path) else Uri.fromFile(java.io.File(path))
                }?.toTypedArray(),
            )
            is Ask.Dialog -> when {
                response is DecisionResponse.DialogAccept && ask.result is JsPromptResult ->
                    ask.result.confirm(response.value ?: "")
                response is DecisionResponse.DialogAccept || response is DecisionResponse.Allow -> ask.result.confirm()
                else -> ask.result.cancel()
            }
            is Ask.Download -> if (response is DecisionResponse.DownloadTo) {
                backend.download(this, ask.id, ask.url, ask.mime, ask.name, response.value)
            }
        }
    }

    // ===================================================================
    // Bridge
    // ===================================================================

    private fun onBridgeMessage(message: WebMessageCompat, origin: String, isMainFrame: Boolean, proxy: JavaScriptReplyProxy) {
        val bridge = spec.bridge ?: return
        if (!isMainFrame && !bridge.allowSubframes) return
        val parsed = Bridge.parse(message.data ?: return) ?: return
        when (parsed) {
            is Bridge.Incoming.Hello -> {
                proxyDocuments[proxy] = documentId
                if (isMainFrame) mainProxy = proxy
            }
            is Bridge.Incoming.Post, is Bridge.Incoming.Invoke -> {
                // Messages from a document that has since been replaced.
                if (proxyDocuments[proxy] != documentId) return
                val channel = (parsed as? Bridge.Incoming.Post)?.channel ?: (parsed as Bridge.Incoming.Invoke).channel
                if (channel !in bridge.channels) return
                val data = (parsed as? Bridge.Incoming.Post)?.data ?: (parsed as Bridge.Incoming.Invoke).data
                var replyId: ULong? = null
                if (parsed is Bridge.Incoming.Invoke) {
                    replyId = requestId()
                    replies[replyId] = proxy to parsed.callId
                }
                emit(ControlEvent.Message(BridgeMessage(channel, origin, isMainFrame, documentId, replyId, data)))
            }
        }
    }

    fun reply(reply: BridgeReply) {
        val (proxy, callId) = replies.remove(reply.replyId) ?: throw BackendException(WebViewError.UnknownRequest)
        if (proxyDocuments[proxy] != documentId) throw BackendException(WebViewError.UnknownRequest)
        proxy.postMessage(Bridge.reply(callId, reply.result))
    }

    fun postMessage(message: OutgoingMessage) {
        if (committedOrigin != message.expectedOrigin) throw BackendException(WebViewError.OriginMismatch)
        val proxy = mainProxy?.takeIf { proxyDocuments[it] == documentId }
            ?: throw BackendException(WebViewError.OriginMismatch)
        proxy.postMessage(Bridge.message(message.channel, message.data))
    }

    fun eval(call: ScriptCall, done: (Result<String>) -> Unit) {
        if (call.world != ScriptWorld.Page) {
            done(Result.failure(BackendException(WebViewError.Unsupported("isolated script world"))))
            return
        }
        if (committedOrigin != call.expectedOrigin) {
            done(Result.failure(BackendException(WebViewError.OriginMismatch)))
            return
        }
        webView.evaluateJavascript(call.source) { result -> done(Result.success(result ?: "null")) }
    }

    // ===================================================================
    // Schemes (network threads)
    // ===================================================================

    fun registerScheme(pending: SchemeServer.Pending): ULong {
        var id = random.nextLong().toULong()
        while (id == 0UL || schemes.putIfAbsent(id, pending) != null) {
            id = random.nextLong().toULong()
        }
        return id
    }

    fun scheme(id: ULong): SchemeServer.Pending =
        schemes[id] ?: throw BackendException(WebViewError.UnknownRequest)

    fun schemeDone(id: ULong) {
        schemes.remove(id)
    }

    // ===================================================================
    // Profile data, misc
    // ===================================================================

    fun snapshot(options: SnapshotOptions): WebSnapshot {
        val w = webView.width.coerceAtLeast(1)
        val h = webView.height.coerceAtLeast(1)
        var bitmap = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888)
        webView.draw(Canvas(bitmap))
        if (options.maxWidth > 0U && w > options.maxWidth.toInt()) {
            val scale = options.maxWidth.toFloat() / w
            bitmap = Bitmap.createScaledBitmap(bitmap, options.maxWidth.toInt(), (h * scale).toInt().coerceAtLeast(1), true)
        }
        val out = ByteArrayOutputStream()
        when (val format = options.format) {
            is SnapshotFormat.Png -> bitmap.compress(Bitmap.CompressFormat.PNG, 100, out)
            is SnapshotFormat.Jpeg -> bitmap.compress(Bitmap.CompressFormat.JPEG, format.value.toInt().coerceIn(0, 100), out)
        }
        return WebSnapshot(bitmap.width.toUInt(), bitmap.height.toUInt(), options.format, out.toByteArray())
    }

    fun find(query: FindQuery, done: (FindResult) -> Unit) {
        if (query.text.isEmpty()) {
            webView.clearMatches()
            done(FindResult(0U, 0U))
            return
        }
        webView.setFindListener { active, matches, finished ->
            if (finished) {
                webView.setFindListener(null)
                done(FindResult(matches.toUInt(), if (matches > 0) (active + 1).toUInt() else 0U))
            }
        }
        webView.findAllAsync(query.text)
    }

    // ===================================================================
    // Close / crash
    // ===================================================================

    /** Deny everything pending: navigation away, crash, close. */
    private fun failPending() {
        val pending = asks.values.toList()
        asks.clear()
        pending.forEach { resolve(it, DecisionResponse.Deny) }
        schemes.values.forEach { it.abort("view closed") }
        schemes.clear()
        replies.clear()
    }

    fun close() {
        if (closed) return
        closed = true
        failPending()
        scriptHandlers.forEach { it.remove() }
        popup?.dismiss()
        dialog?.setOnCancelListener(null)
        dialog?.dismiss()
        (webView.parent as? ViewGroup)?.removeView(webView)
        webView.stopLoading()
        webView.destroy()
        control.close()
        telemetrySubs.keys.forEach { it.close() }
        if (deleteProfileOnClose && profileName != null) backend.deleteProfile(profileName)
    }

    // ===================================================================
    // Clients
    // ===================================================================

    private inner class Client : WebViewClient() {
        override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
            if (!request.isForMainFrame) return false
            return !admit(request.url.toString(), request.hasGesture())
        }

        override fun onPageStarted(view: WebView, url: String, favicon: Bitmap?) {
            emit(ControlEvent.NavigationStarted(NavInfo(nextNavigation(), url, false, false)))
            if (wants(TelemetryKind.Progress)) telemetry(TelemetryKind.Progress, TelemetryEvent.Progress(0f))
        }

        override fun onPageCommitVisible(view: WebView, url: String) {
            documentId++
            committedOrigin = CompiledRules.originOf(url)
            mainProxy = null
            emit(ControlEvent.NavigationCommitted(NavInfo(navigationId, url, false, false)))
            if (wants(TelemetryKind.Url)) telemetry(TelemetryKind.Url, TelemetryEvent.Url(url))
        }

        override fun onPageFinished(view: WebView, url: String) {
            emit(ControlEvent.NavigationFinished(NavInfo(navigationId, url, false, false)))
            injectDocumentEnd(url)
            if (wants(TelemetryKind.History)) {
                telemetry(TelemetryKind.History, TelemetryEvent.History(HistoryState(view.canGoBack(), view.canGoForward())))
            }
        }

        override fun onReceivedError(view: WebView, request: WebResourceRequest, error: WebResourceError) {
            if (!request.isForMainFrame) return
            val kind = when (error.errorCode) {
                ERROR_HOST_LOOKUP -> NavErrorKind.Dns
                ERROR_TIMEOUT -> NavErrorKind.Timeout
                ERROR_FAILED_SSL_HANDSHAKE -> NavErrorKind.Tls
                ERROR_BAD_URL, ERROR_UNSUPPORTED_SCHEME -> NavErrorKind.BadUrl
                ERROR_CONNECT, ERROR_IO -> NavErrorKind.Network
                else -> NavErrorKind.Other
            }
            emit(ControlEvent.NavigationFailed(NavFailure(navigationId, request.url.toString(), kind, error.description.toString())))
        }

        override fun onReceivedSslError(view: WebView, handler: SslErrorHandler, error: SslError) {
            val cert = error.certificate
            val fingerprint = try {
                val der = cert.x509Certificate?.encoded
                der?.let { MessageDigest.getInstance("SHA-256").digest(it).joinToString("") { b -> "%02x".format(b) } } ?: ""
            } catch (_: Exception) {
                ""
            }
            val host = Uri.parse(error.url).host ?: ""
            ask(Ask.Ssl(handler), DecisionKind.TlsError(TlsErrorInfo(error.url, host, "certificate error ${error.primaryError}", fingerprint)), null)
        }

        override fun onReceivedHttpAuthRequest(view: WebView, handler: HttpAuthHandler, host: String, realm: String) {
            ask(Ask.Auth(handler), DecisionKind.HttpAuth(HttpAuthAsk(host, realm, false)), null)
        }

        override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? =
            backend.schemes.intercept(this@ManagedView, request)

        override fun onRenderProcessGone(view: WebView, detail: RenderProcessGoneDetail): Boolean {
            val reason = if (detail.didCrash()) TerminationReason.Crashed else TerminationReason.Killed
            emit(ControlEvent.ProcessTerminated(TerminationInfo(reason)))
            // Returning true keeps the app alive; the view is unusable,
            // so it is torn down and the app reopens one if it wants.
            backend.closeAfterCrash(this@ManagedView)
            return true
        }
    }

    private fun injectDocumentEnd(url: String) {
        val origin = CompiledRules.originOf(url)
        for (script in spec.userScripts) {
            if (script.injectAt != InjectionTime.DocumentEnd || script.world != ScriptWorld.Page) continue
            if (script.allowedOrigins.isNotEmpty() && origin !in script.allowedOrigins) continue
            webView.evaluateJavascript(script.source, null)
        }
    }

    private inner class Chrome : WebChromeClient() {
        override fun onProgressChanged(view: WebView, newProgress: Int) {
            // Coalesce: one event per percent step at most.
            if (newProgress == lastProgress || !wants(TelemetryKind.Progress)) return
            lastProgress = newProgress
            telemetry(TelemetryKind.Progress, TelemetryEvent.Progress(newProgress / 100f))
        }

        override fun onReceivedTitle(view: WebView, title: String?) {
            if (wants(TelemetryKind.Title)) telemetry(TelemetryKind.Title, TelemetryEvent.Title(title ?: ""))
        }

        override fun onConsoleMessage(message: ConsoleMessage): Boolean {
            if (!backend.config.allowInspector || !wants(TelemetryKind.Console)) return false
            val level = when (message.messageLevel()) {
                ConsoleMessage.MessageLevel.DEBUG, ConsoleMessage.MessageLevel.TIP -> ConsoleLevel.Debug
                ConsoleMessage.MessageLevel.WARNING -> ConsoleLevel.Warning
                ConsoleMessage.MessageLevel.ERROR -> ConsoleLevel.Error
                else -> ConsoleLevel.Log
            }
            telemetry(TelemetryKind.Console, TelemetryEvent.Console(ConsoleLine(level, message.message(), message.sourceId() ?: "", message.lineNumber().toUInt())))
            return true
        }

        override fun onCreateWindow(view: WebView, isDialog: Boolean, isUserGesture: Boolean, resultMsg: Message): Boolean {
            val hit = view.hitTestResult.extra ?: ""
            if (hit.isNotEmpty() && rules.evaluate(hit, spec.settings.fileAccess) == RuleAction.Deny) return false
            ask(Ask.NewWindow(resultMsg, hit), DecisionKind.NewWindow(NewWindowAsk(hit, isUserGesture)), spec.timeouts.navigationMs.toLong())
            return true
        }

        override fun onCloseWindow(window: WebView) {
            emit(ControlEvent.CloseRequested)
        }

        override fun onPermissionRequest(request: PermissionRequest) {
            val origin = request.origin.toString().trimEnd('/')
            ask(
                Ask.Permission(request),
                DecisionKind.Permission(PermissionAsk(request.resources.map(::webPermission).distinct(), origin, committedOrigin ?: origin)),
                null,
            )
        }

        override fun onGeolocationPermissionsShowPrompt(origin: String, callback: GeolocationPermissions.Callback) {
            val o = origin.trimEnd('/')
            ask(Ask.Geolocation(callback, origin), DecisionKind.Permission(PermissionAsk(listOf(WebPermission.Geolocation), o, committedOrigin ?: o)), null)
        }

        override fun onShowFileChooser(view: WebView, callback: ValueCallback<Array<Uri>>, params: FileChooserParams): Boolean {
            ask(
                Ask.Files(callback),
                DecisionKind.FileChooser(FileChooserAsk(params.mode == FileChooserParams.MODE_OPEN_MULTIPLE, params.acceptTypes.filter { it.isNotEmpty() }, params.isCaptureEnabled)),
                null,
            )
            return true
        }

        override fun onJsAlert(view: WebView, url: String, message: String, result: JsResult): Boolean =
            dialog(DialogKind.Alert, url, message, "", result)

        override fun onJsConfirm(view: WebView, url: String, message: String, result: JsResult): Boolean =
            dialog(DialogKind.Confirm, url, message, "", result)

        override fun onJsPrompt(view: WebView, url: String, message: String, defaultValue: String?, result: JsPromptResult): Boolean =
            dialog(DialogKind.Prompt, url, message, defaultValue ?: "", result)

        override fun onJsBeforeUnload(view: WebView, url: String, message: String, result: JsResult): Boolean =
            dialog(DialogKind.BeforeUnload, url, message, "", result)

        private fun dialog(kind: DialogKind, url: String, message: String, default: String, result: JsResult): Boolean {
            ask(Ask.Dialog(result), DecisionKind.Dialog(DialogAsk(kind, message, default, CompiledRules.originOf(url) ?: "")), null)
            return true
        }

        override fun onShowCustomView(view: View, callback: CustomViewCallback) {
            emit(ControlEvent.FullscreenChanged(true))
            callback.onCustomViewHidden()
        }

        override fun onHideCustomView() {
            emit(ControlEvent.FullscreenChanged(false))
        }
    }

    private fun onDownload(url: String, contentDisposition: String?, mime: String?, length: Long) {
        val name = android.webkit.URLUtil.guessFileName(url, contentDisposition, mime)
            .replace('/', '_').replace('\\', '_')
        val id = requestId()
        asks[id] = Ask.Download(id, url, mime ?: "application/octet-stream", name)
        emit(
            ControlEvent.DecisionRequested(
                DecisionRequest(id, DecisionKind.Download(DownloadAsk(id, url, name, mime ?: "", if (length >= 0) length.toULong() else null))),
            ),
        )
    }

    companion object {
        const val TELEMETRY_DEPTH = 64

        fun webPermission(resource: String): WebPermission = when (resource) {
            PermissionRequest.RESOURCE_VIDEO_CAPTURE -> WebPermission.Camera
            PermissionRequest.RESOURCE_AUDIO_CAPTURE -> WebPermission.Microphone
            PermissionRequest.RESOURCE_MIDI_SYSEX -> WebPermission.Midi
            PermissionRequest.RESOURCE_PROTECTED_MEDIA_ID -> WebPermission.ProtectedMedia
            else -> WebPermission.Other
        }
    }
}
