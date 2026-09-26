package dev.istmo.gradle

import groovy.json.JsonSlurper
import org.gradle.api.GradleException
import java.io.File

/** App identity from istmo.toml `[app]`, resolved by `istmo-build`. */
data class AppIdentity(
    val id: String?,
    val name: String,
    val versionName: String,
    val versionCode: Int,
    val minSdk: Int?,
    /** Drawable reference (`@mipmap/istmo_launcher`) when `[app] icon` is set. */
    val icon: String?,
)

/** One `[[gradle]]` dependency aggregated across the app and its plugins. */
data class GradleDependency(val scope: String, val notation: String)

/**
 * `android/.istmo/istmo.json`, written by the app crate's `build.rs`
 * (`istmo_build::emit()`) whenever it compiles for Android.
 */
data class IstmoMetadata(
    /** Raw file contents, compared after cargo runs to detect plugin-set changes. */
    val text: String,
    val packageName: String,
    val libName: String,
    val manifestPath: File,
    val app: AppIdentity,
    val plugins: List<LinkedPlugin>,
    val gradle: List<GradleDependency>,
) {
    companion object {
        const val SCHEMA = 1

        /** Relative to the Android project root. */
        const val RELATIVE_PATH = ".istmo/istmo.json"

        /** Build-script env var whose change forces `istmo-build` to rewrite the metadata. */
        const val REFRESH_ENV = "ISTMO_METADATA_REFRESH"

        fun parse(file: File): IstmoMetadata {
            val text = file.readText()
            val root = JsonSlurper().parseText(text) as? Map<*, *>
                ?: throw GradleException("istmo: ${file.path} is not a JSON object")
            val schema = (root["schema"] as? Number)?.toInt()
            if (schema != SCHEMA) {
                throw GradleException(
                    "istmo: ${file.path} has schema $schema but this Gradle plugin reads schema $SCHEMA; " +
                        "use matching versions of istmo-build and dev.istmo.app",
                )
            }
            val app = root["app"] as Map<*, *>
            return IstmoMetadata(
                text = text,
                packageName = root["package"] as String,
                libName = root["lib_name"] as String,
                manifestPath = File(root["manifest_path"] as String),
                app = AppIdentity(
                    id = app["id"] as String?,
                    name = app["name"] as String,
                    versionName = app["version_name"] as String,
                    versionCode = (app["version_code"] as Number).toInt(),
                    minSdk = (app["min_sdk"] as Number?)?.toInt(),
                    icon = app["icon"] as String?,
                ),
                plugins = (root["plugins"] as List<*>).map { raw ->
                    val plugin = raw as Map<*, *>
                    LinkedPlugin(
                        crateName = plugin["id"] as String,
                        nativeDir = File(plugin["native_dir"] as String),
                        manifest = (plugin["manifest"] as String?)?.let(::File),
                        resDir = (plugin["res"] as String?)?.let(::File),
                        minAndroidApi = (plugin["min_sdk"] as Number?)?.toInt(),
                    )
                },
                gradle = (root["gradle"] as List<*>).map { raw ->
                    val dep = raw as Map<*, *>
                    GradleDependency(dep["scope"] as String, dep["notation"] as String)
                },
            )
        }
    }
}

/**
 * Loads [IstmoMetadata] for the crate at [crateDir], refreshing it first
 * with `cargo check` when it is missing or older than the files that
 * decide the plugin set (`Cargo.toml`, `Cargo.lock`, `istmo.toml`).
 */
internal class MetadataStore(
    private val androidRoot: File,
    private val crateDir: File,
    private val toolchain: Toolchain,
) {
    val file: File = File(androidRoot, IstmoMetadata.RELATIVE_PATH)

    fun load(rustTarget: String, minSdk: Int): IstmoMetadata {
        if (isStale()) refresh(rustTarget, minSdk)
        if (!file.isFile) {
            throw GradleException(
                "istmo: `cargo check` did not write ${file.path}. Make sure the crate's build.rs calls " +
                    "`istmo_build::emit()` and that istmo-build matches this Gradle plugin's version.",
            )
        }
        return IstmoMetadata.parse(file)
    }

    private fun isStale(): Boolean {
        if (!file.isFile) return true
        val stamp = file.lastModified()
        return inputs().any { it.isFile && it.lastModified() > stamp }
    }

    private fun inputs(): List<File> {
        val lock = generateSequence(crateDir) { it.parentFile }
            .map { File(it, "Cargo.lock") }
            .firstOrNull { it.isFile }
        return listOfNotNull(File(crateDir, "Cargo.toml"), File(crateDir, "istmo.toml"), lock)
    }

    private fun refresh(rustTarget: String, minSdk: Int) {
        toolchain.requireReady(listOf(rustTarget))
        val manifest = File(crateDir, "Cargo.toml")
        toolchain.project.logger.lifecycle("istmo: refreshing plugin metadata (cargo check --target $rustTarget)")
        val env = toolchain.cargoEnvironment(rustTarget, minSdk) +
            (IstmoMetadata.REFRESH_ENV to System.nanoTime().toString())
        val result = toolchain.runCargo(
            listOf("check", "--manifest-path", manifest.path, "--lib", "--target", rustTarget),
            env,
        )
        if (result.exitCode != 0) {
            throw GradleException(
                "istmo: `cargo check --target $rustTarget` failed while refreshing plugin metadata:\n" +
                    result.stderr.takeLast(8000),
            )
        }
        // istmo-build skips rewriting an unchanged file; bump the stamp so the
        // staleness check does not trigger again on the next configuration.
        if (file.isFile) file.setLastModified(System.currentTimeMillis())
    }
}
