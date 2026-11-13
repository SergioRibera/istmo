package dev.istmo.plugins.datastore

import android.content.Context
import android.content.SharedPreferences
import android.util.Base64
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.DataStoreBackend
import dev.istmo.runtime.DataStoreConfig
import dev.istmo.runtime.DataStoreError
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
 * Earlier versions did use the bare namespace. On first open of the
 * prefixed file, entries of such a legacy file are copied into it and
 * the legacy file is deleted. The migration is skipped when the
 * namespace is the app's default preferences name — that file is the
 * app's, so the namespace starts empty instead. An app that also wrote
 * a preferences file of the same name as a namespace itself must not
 * rely on that file surviving the first open after the upgrade.
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
        /** Serialises the exists check and the legacy migration across instances. */
        private val openLock = Any()

        fun openPrefs(context: Context, namespace: String): SharedPreferences {
            // The namespace becomes part of a file name.
            if (namespace.isEmpty() || namespace.contains('/') || namespace.contains(File.separatorChar)) {
                throw BackendException(DataStoreError.Backend("invalid namespace `$namespace`"))
            }
            val name = "${context.packageName}.istmo.$namespace"
            return synchronized(openLock) {
                val prefsDir = File(context.applicationInfo.dataDir, "shared_prefs")
                val firstOpen = !File(prefsDir, "$name.xml").exists()
                val prefs = try {
                    context.getSharedPreferences(name, Context.MODE_PRIVATE)
                } catch (e: IllegalArgumentException) {
                    throw BackendException(DataStoreError.Backend("invalid namespace `$namespace`: ${e.message}"))
                }
                if (firstOpen && namespace != "${context.packageName}_preferences") {
                    migrateLegacy(context, prefsDir, namespace, prefs)
                }
                prefs
            }
        }

        /** Copy the unprefixed legacy file `legacyName` into [target], then delete it. */
        private fun migrateLegacy(context: Context, prefsDir: File, legacyName: String, target: SharedPreferences) {
            val legacyFile = File(prefsDir, "$legacyName.xml")
            if (!legacyFile.exists()) return
            val legacy = context.getSharedPreferences(legacyName, Context.MODE_PRIVATE)
            val entries = legacy.all
            if (entries.isNotEmpty()) {
                val editor = target.edit()
                for ((key, value) in entries) {
                    when (value) {
                        is String -> editor.putString(key, value)
                        is Long -> editor.putLong(key, value)
                        is Int -> editor.putInt(key, value)
                        is Boolean -> editor.putBoolean(key, value)
                        is Float -> editor.putFloat(key, value)
                        is Set<*> -> editor.putStringSet(key, value.filterIsInstance<String>().toSet())
                    }
                }
                // Keep the legacy file when the copy did not reach disk,
                // so the values are not lost; the next open sees the
                // prefixed file missing and retries.
                if (!editor.commit()) return
            }
            legacy.edit().clear().commit()
            legacyFile.delete()
            File(prefsDir, "$legacyName.xml.bak").delete()
        }
    }
}
