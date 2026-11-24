package dev.istmo.plugins.webview

import android.content.res.AssetManager
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import dev.istmo.runtime.BackendException
import java.io.IOException
import java.io.InputStream
import java.util.concurrent.CompletableFuture
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

/**
 * Custom schemes on Android. WebView cannot register new schemes for
 * fetch/XHR, so scheme `name` is served from
 * `https://appassets.androidplatform.net/<name>/…` — the host
 * `WebViewAssetLoader` reserves for app content (it never reaches the
 * network). Pages get that origin; `capabilities().schemeOrigins`
 * reports it.
 *
 * Only the view's own main-frame loads and documents already on that
 * origin may read it, so a page in a browser view cannot fetch app
 * assets.
 */
internal class SchemeServer(
    private val assets: AssetManager,
    private val schemes: List<CustomScheme>,
) {
    /** A dynamic request waiting for the app. */
    class Pending {
        val head = CompletableFuture<SchemeResponseHead>()
        val body = ChunkStream()

        fun abort(reason: String) {
            head.completeExceptionally(IOException(reason))
            body.fail(reason)
        }
    }

    fun intercept(view: ManagedView, request: WebResourceRequest): WebResourceResponse? {
        val url = request.url
        if (url.scheme != "https" || url.host != HOST) return null
        val segments = url.pathSegments
        val scheme = schemes.firstOrNull { it.name == segments.firstOrNull() }
            ?: return error(404, "Not Found")
        if (!request.isForMainFrame && view.committedOrigin != ORIGIN) {
            return error(403, "Forbidden")
        }
        val rest = segments.drop(1)
        return when (val source = scheme.source) {
            is SchemeSource.Assets -> asset(source.value.dir, rest)
            is SchemeSource.Dynamic -> dynamic(view, scheme.name, request)
        }
    }

    private fun asset(dir: String, rest: List<String>): WebResourceResponse {
        // Decoded segments: refuse anything that could leave `dir`.
        if (rest.any { it.isEmpty() || it == "." || it == ".." || it.contains('\\') || it.contains('\u0000') || it.contains('/') }) {
            return error(400, "Bad Request")
        }
        val path = (listOf(dir.trim('/')) + rest.ifEmpty { listOf("index.html") }).filter { it.isNotEmpty() }.joinToString("/")
        val mime = mimeFor(path) ?: return error(415, "Unsupported Media Type")
        val stream = try {
            assets.open(path, AssetManager.ACCESS_STREAMING)
        } catch (_: IOException) {
            return error(404, "Not Found")
        }
        return WebResourceResponse(mime, null, 200, "OK", SAFE_HEADERS, stream)
    }

    private fun dynamic(view: ManagedView, scheme: String, request: WebResourceRequest): WebResourceResponse {
        val pending = Pending()
        val id = view.registerScheme(pending)
        view.emit(
            ControlEvent.SchemeRequest(
                SchemeRequestInfo(
                    id,
                    scheme,
                    request.method,
                    request.url.toString(),
                    request.requestHeaders.map { HttpHeader(it.key, it.value) },
                    false,
                    view.committedOrigin,
                ),
            ),
        )
        // The status line has to exist before WebView reads the body, so
        // this network thread waits for the head (bounded); the body
        // then streams as chunks arrive.
        val head = try {
            pending.head.get(view.spec.timeouts.schemeHeadMs.toLong().coerceAtLeast(1), TimeUnit.MILLISECONDS)
        } catch (_: TimeoutException) {
            view.schemeDone(id)
            return error(504, "Gateway Timeout")
        } catch (_: Exception) {
            view.schemeDone(id)
            return error(502, "Bad Gateway")
        }
        val headers = SAFE_HEADERS + head.headers
            .filter { !it.name.equals("Access-Control-Allow-Origin", ignoreCase = true) || it.value != "*" }
            .associate { it.name to it.value }
        return WebResourceResponse(head.mimeType, null, head.status.toInt(), reason(head.status.toInt()), headers, pending.body)
    }

    /** `scheme_chunk` from the app (IO dispatcher: may wait for room). */
    fun chunk(view: ManagedView, chunk: SchemeChunk) {
        val pending = view.scheme(chunk.requestId)
        when (val body = chunk.body) {
            is SchemeBody.Data -> pending.body.put(body.value)
            is SchemeBody.End -> {
                pending.body.end()
                view.schemeDone(chunk.requestId)
            }
            is SchemeBody.Abort -> {
                pending.abort(body.value)
                view.schemeDone(chunk.requestId)
            }
        }
    }

    fun head(view: ManagedView, head: SchemeResponseHead) {
        if (!view.scheme(head.requestId).head.complete(head)) {
            throw BackendException(WebViewError.UnknownRequest)
        }
    }

    private fun error(status: Int, reason: String) =
        WebResourceResponse("text/plain", "utf-8", status, reason, SAFE_HEADERS, ByteArray(0).inputStream())

    companion object {
        const val HOST = "appassets.androidplatform.net"
        const val ORIGIN = "https://$HOST"

        private val SAFE_HEADERS = mapOf(
            "X-Content-Type-Options" to "nosniff",
            "Cache-Control" to "no-cache",
        )

        private val MIME = mapOf(
            "html" to "text/html", "htm" to "text/html", "css" to "text/css",
            "js" to "text/javascript", "mjs" to "text/javascript", "json" to "application/json",
            "wasm" to "application/wasm", "svg" to "image/svg+xml", "png" to "image/png",
            "jpg" to "image/jpeg", "jpeg" to "image/jpeg", "gif" to "image/gif", "webp" to "image/webp",
            "avif" to "image/avif", "ico" to "image/x-icon", "woff" to "font/woff", "woff2" to "font/woff2",
            "ttf" to "font/ttf", "otf" to "font/otf", "txt" to "text/plain", "xml" to "application/xml",
            "mp4" to "video/mp4", "webm" to "video/webm", "mp3" to "audio/mpeg", "ogg" to "audio/ogg",
            "wav" to "audio/wav", "pdf" to "application/pdf", "map" to "application/json",
        )

        fun mimeFor(path: String): String? = MIME[path.substringAfterLast('.', "").lowercase()]

        private fun reason(status: Int) = when (status) {
            200 -> "OK"
            201 -> "Created"
            204 -> "No Content"
            206 -> "Partial Content"
            301 -> "Moved Permanently"
            302 -> "Found"
            304 -> "Not Modified"
            400 -> "Bad Request"
            401 -> "Unauthorized"
            403 -> "Forbidden"
            404 -> "Not Found"
            500 -> "Internal Server Error"
            else -> "Status"
        }
    }
}

/**
 * Body fed by `scheme_chunk` calls, read by WebView's network thread.
 * A bounded queue gives backpressure: the app's writer waits while the
 * page is not reading.
 */
internal class ChunkStream : InputStream() {
    private val queue = LinkedBlockingQueue<ByteArray>(16)
    private var current: ByteArray = ByteArray(0)
    private var offset = 0

    @Volatile
    private var failure: String? = null

    fun put(data: ByteArray) {
        if (data.isEmpty()) return
        // Wait for room, but give up once the reader is gone.
        while (!queue.offer(data, 100, TimeUnit.MILLISECONDS)) {
            failure?.let { throw BackendException(WebViewError.UnknownRequest) }
        }
    }

    fun end() = queue.put(END)

    fun fail(reason: String) {
        failure = reason
        queue.clear()
        queue.offer(END)
    }

    override fun read(): Int {
        val one = ByteArray(1)
        return if (read(one, 0, 1) == -1) -1 else one[0].toInt() and 0xff
    }

    override fun read(b: ByteArray, off: Int, len: Int): Int {
        if (len == 0) return 0
        while (offset >= current.size) {
            val next = queue.take()
            failure?.let { throw IOException(it) }
            if (next === END) {
                queue.offer(END)
                return -1
            }
            current = next
            offset = 0
        }
        val n = minOf(len, current.size - offset)
        System.arraycopy(current, offset, b, off, n)
        offset += n
        return n
    }

    override fun close() {
        fail("closed")
    }

    private companion object {
        val END = ByteArray(0)
    }
}
