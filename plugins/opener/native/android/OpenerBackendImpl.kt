package dev.istmo.plugins.opener

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.OpenError
import dev.istmo.runtime.OpenerBackend

/** Reference backend for `istmo.opener`: an `ACTION_VIEW` intent. */
class OpenerBackendImpl(
    private val context: Context,
) : OpenerBackend {

    override suspend fun open(url: String) {
        val uri = runCatching { Uri.parse(url) }.getOrNull()
        if (uri == null || uri.scheme.isNullOrEmpty()) {
            throw BackendException(OpenError.Invalid)
        }
        val intent = Intent(Intent.ACTION_VIEW, uri).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        try {
            context.startActivity(intent)
        } catch (e: ActivityNotFoundException) {
            throw BackendException(OpenError.NoHandler)
        } catch (e: SecurityException) {
            throw BackendException(OpenError.Backend(e.message ?: "not allowed"))
        }
    }
}
