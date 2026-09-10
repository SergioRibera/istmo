package dev.istmo.runtime

import android.content.Context
import android.content.SharedPreferences
import android.util.Base64

/**
 * Android impl of the codegen `DataStoreBackend` interface, backed by
 * `SharedPreferences`.
 *
 * SharedPreferences is chosen over Jetpack DataStore because the surface
 * this plugin exposes is a plain synchronous KV store — no observers, no
 * migrations, no schema. DataStore's Flow-based API would add complexity
 * without matching what the wire contract needs. SharedPreferences also
 * ships with the platform, so the plugin declares zero Gradle deps.
 *
 * Byte payloads are stored as Base64 strings, since SharedPreferences
 * cannot hold raw `ByteArray` values.
 */
class DataStoreBackendImpl(context: Context, config: DataStoreConfig) : DataStoreBackend {

    private val prefs: SharedPreferences =
        context.getSharedPreferences(config.namespace, Context.MODE_PRIVATE)

    override suspend fun get_string(key: String): String? =
        if (prefs.contains(key)) prefs.getString(key, null) else null

    override suspend fun set_string(key: String, value: String) {
        commit { it.putString(key, value) }
    }

    override suspend fun get_i64(key: String): Long? =
        if (prefs.contains(key)) prefs.getLong(key, 0L) else null

    override suspend fun set_i64(key: String, value: Long) {
        commit { it.putLong(key, value) }
    }

    override suspend fun get_f64(key: String): Double? {
        if (!prefs.contains(key)) return null
        // SharedPreferences has no Double API; store the raw bit pattern
        // under a Long so the round trip is exact.
        val bits = prefs.getLong(key, 0L)
        return Double.fromBits(bits)
    }

    override suspend fun set_f64(key: String, value: Double) {
        commit { it.putLong(key, value.toRawBits()) }
    }

    override suspend fun get_bool(key: String): Boolean? =
        if (prefs.contains(key)) prefs.getBoolean(key, false) else null

    override suspend fun set_bool(key: String, value: Boolean) {
        commit { it.putBoolean(key, value) }
    }

    override suspend fun get_bytes(key: String): ByteArray? {
        val encoded = if (prefs.contains(key)) prefs.getString(key, null) else null
        if (encoded == null) return null
        return try {
            Base64.decode(encoded, Base64.NO_WRAP)
        } catch (e: IllegalArgumentException) {
            throw BackendException(DataStoreError.Corrupted("base64 decode: ${e.message}"))
        }
    }

    override suspend fun set_bytes(key: String, value: ByteArray) {
        val encoded = Base64.encodeToString(value, Base64.NO_WRAP)
        commit { it.putString(key, encoded) }
    }

    override suspend fun remove(key: String): Boolean {
        if (!prefs.contains(key)) return false
        commit { it.remove(key) }
        return true
    }

    override suspend fun contains(key: String): Boolean = prefs.contains(key)

    override suspend fun keys(): List<String> = prefs.all.keys.toList()

    override suspend fun clear() {
        commit { it.clear() }
    }

    private inline fun commit(edit: (SharedPreferences.Editor) -> Unit) {
        val editor = prefs.edit()
        edit(editor)
        if (!editor.commit()) {
            throw BackendException(DataStoreError.Backend("SharedPreferences commit failed"))
        }
    }
}
