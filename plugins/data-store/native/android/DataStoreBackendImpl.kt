package dev.istmo.plugins.datastore

import android.content.Context
import android.content.SharedPreferences
import android.util.Base64
import dev.istmo.runtime.BackendException
import java.io.File

/**
 * `SharedPreferences` backend: one preferences file per namespace.
 *
 * The file is named `<package name>.istmo.<namespace>`, like the iOS
 * suite. The bare namespace is never the file name: a namespace equal
 * to `<package name>_preferences` (or to any file the app writes
 * itself) would otherwise be the app's own preferences, and `clear()`
 * would wipe them.
 *
 * A key read as the wrong type fails with `DataStoreError.Corrupted`,
 * like the desktop backend. `SharedPreferences` has no `f64` or bytes
 * type, so `f64` values are stored as their `Long` bit pattern and
 * bytes as Base64 strings; reading an `f64` key with `get_i64` (or a
 * bytes key with `get_string`) therefore cannot be told apart from a
 * real value of that type.
 */
class DataStoreBackendImpl(context: Context, config: DataStoreConfig) : DataStoreBackend {

    private val prefs: SharedPreferences = openPrefs(context, config.namespace)

    override suspend fun get_string(key: String): String? =
        typed(key, "string") { prefs.getString(key, null) }

    override suspend fun set_string(key: String, value: String) {
        commit { it.putString(key, value) }
    }

    override suspend fun get_i64(key: String): Long? =
        typed(key, "i64") { prefs.getLong(key, 0L) }

    override suspend fun set_i64(key: String, value: Long) {
        commit { it.putLong(key, value) }
    }

    override suspend fun get_f64(key: String): Double? =
        typed(key, "f64") { Double.fromBits(prefs.getLong(key, 0L)) }

    override suspend fun set_f64(key: String, value: Double) {
        commit { it.putLong(key, value.toRawBits()) }
    }

    override suspend fun get_bool(key: String): Boolean? =
        typed(key, "bool") { prefs.getBoolean(key, false) }

    override suspend fun set_bool(key: String, value: Boolean) {
        commit { it.putBoolean(key, value) }
    }

    override suspend fun get_bytes(key: String): ByteArray? {
        val encoded = typed(key, "bytes") { prefs.getString(key, null) } ?: return null
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

    /** `null` for a missing key; `Corrupted` when the stored value has another type. */
    private inline fun <T> typed(key: String, wanted: String, read: () -> T): T? {
        if (!prefs.contains(key)) return null
        return try {
            read()
        } catch (e: ClassCastException) {
            throw BackendException(
                DataStoreError.Corrupted("key `$key` holds another type but caller asked for $wanted"),
            )
        }
    }

    private inline fun commit(edit: (SharedPreferences.Editor) -> Unit) {
        val editor = prefs.edit()
        edit(editor)
        if (!editor.commit()) {
            throw BackendException(DataStoreError.Backend("SharedPreferences commit failed"))
        }
    }

    private companion object {
        fun openPrefs(context: Context, namespace: String): SharedPreferences {
            // The namespace becomes part of a file name.
            if (namespace.isEmpty() || namespace.contains('/') || namespace.contains(File.separatorChar)) {
                throw BackendException(DataStoreError.Backend("invalid namespace `$namespace`"))
            }
            val name = "${context.packageName}.istmo.$namespace"
            return try {
                context.getSharedPreferences(name, Context.MODE_PRIVATE)
            } catch (e: IllegalArgumentException) {
                throw BackendException(DataStoreError.Backend("invalid namespace `$namespace`: ${e.message}"))
            }
        }
    }
}
