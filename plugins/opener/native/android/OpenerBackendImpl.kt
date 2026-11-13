package dev.istmo.plugins.opener

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.FileUriExposedException
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.OpenError
import dev.istmo.runtime.OpenerBackend

/**
 * Reference backend for `istmo.opener`: an `ACTION_VIEW` intent.
 *
 * Only URLs whose scheme is in [allowedSchemes] are opened, mirroring
 * the Rust-side `SchemePolicy`; anything else fails with
 * `OpenError.SchemeNotAllowed`. The intent is restricted to
 * `CATEGORY_BROWSABLE` activities — the ones that declared they are
 * safe to launch from an untrusted link — so a URL cannot reach
 * exported screens that were never meant to be opened from one.
 */
class OpenerBackendImpl(
    private val context: Context,
    allowedSchemes: Set<String> = DEFAULT_ALLOWED_SCHEMES,
) : OpenerBackend {

    private val allowedSchemes: Set<String> = allowedSchemes.map { it.lowercase() }.toSet()

    override suspend fun open(url: String) {
        if (url.isEmpty() || url.startsWith("-") || hasControlOrFormat(url)) {
            throw BackendException(OpenError.Invalid)
        }
        // Intent filters match schemes case-sensitively and declare them
        // lowercase, so `HTTPS://…` must be lowercased to find a browser.
        val uri = runCatching { Uri.parse(url).normalizeScheme() }.getOrNull()
        val scheme = uri?.scheme?.lowercase()
        if (uri == null || scheme.isNullOrEmpty()) {
            throw BackendException(OpenError.Invalid)
        }
        if (scheme !in allowedSchemes) {
            throw BackendException(OpenError.SchemeNotAllowed(scheme))
        }
        val intent = Intent(Intent.ACTION_VIEW, uri)
            .addCategory(Intent.CATEGORY_BROWSABLE)
            // Started from the application context, which has no task.
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        try {
            context.startActivity(intent)
        } catch (e: ActivityNotFoundException) {
            throw BackendException(OpenError.NoHandler)
        } catch (e: SecurityException) {
            throw BackendException(OpenError.Backend(e.message ?: "not allowed"))
        } catch (e: RuntimeException) {
            // A `file://` URI (only reachable when "file" is allowed)
            // is refused by StrictMode on API 24+; no app can open it.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N && e is FileUriExposedException) {
                throw BackendException(OpenError.NoHandler)
            }
            throw e
        }
    }

    companion object {
        /** Same list as the Rust `DEFAULT_ALLOWED_SCHEMES`. */
        val DEFAULT_ALLOWED_SCHEMES: Set<String> =
            setOf("http", "https", "mailto", "tel", "sms", "geo")

        /**
         * Control characters (Cc) or invisible format characters (Cf:
         * zero-width spaces, bidi overrides and isolates, BOM, tags),
         * which make a URL read differently from where it goes. Walks
         * code points so supplementary-plane Cf characters are seen.
         */
        private fun hasControlOrFormat(url: String): Boolean {
            var i = 0
            while (i < url.length) {
                val cp = url.codePointAt(i)
                val type = Character.getType(cp)
                if (type == Character.CONTROL.toInt() || type == Character.FORMAT.toInt()) return true
                i += Character.charCount(cp)
            }
            return false
        }
    }
}
