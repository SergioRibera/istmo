package dev.istmo.gradle

import com.android.build.api.variant.AndroidComponentsExtension
import com.android.build.api.variant.Variant
import com.android.build.gradle.BaseExtension
import org.gradle.api.GradleException
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.api.provider.ListProperty
import org.gradle.api.provider.Property
import org.tomlj.Toml
import java.io.File

open class IstmoLoaderExtension(project: Project) {
    val workspaceRoot: Property<File> =
        project.objects.property(File::class.java)

    val excludePlugins: ListProperty<String> =
        project.objects.listProperty(String::class.java).convention(emptyList())
}

/**
 * One istmo plugin crate linked into the app.
 *
 * @property nativeDir `native/android/` — Kotlin sources.
 * @property manifest `native/android/AndroidManifest.xml` when present:
 *   merged into every variant's manifest (providers, receivers,
 *   intent filters, permissions, `<queries>`).
 * @property resDir `native/android/res/` when present: added as a
 *   resource source directory (e.g. `xml/file_paths.xml`).
 * @property minAndroidApi `[min_versions] android` from `istmo.toml`.
 */
data class LinkedPlugin(
    val crateName: String,
    val nativeDir: File,
    val manifest: File?,
    val resDir: File?,
    val minAndroidApi: Int?,
)

/**
 * Auto-linking Gradle plugin for istmo apps. Walks upward from the
 * consumer app's `project.rootDir` until it finds a Cargo workspace
 * root (a `Cargo.toml` with a `[workspace]` table), enumerates the
 * workspace's plugin crates by looking at each member's `istmo.toml`
 * + `native/android/` sibling, and:
 *
 * - adds every existing directory to the Android app's `main` Kotlin
 *   source set;
 * - merges each plugin's `native/android/AndroidManifest.xml` into
 *   every variant's manifest and adds `native/android/res/` as a
 *   resource directory (AGP 8.3+ Variant API);
 * - fails the build when a plugin's `[min_versions] android` is newer
 *   than the variant's `minSdk`.
 *
 * No files are copied.
 */
class IstmoLoaderPlugin : Plugin<Project> {

    override fun apply(project: Project) {
        val extension: IstmoLoaderExtension =
            project.extensions.create("istmo", IstmoLoaderExtension::class.java, project)

        // Variant callbacks must be registered before AGP computes the
        // variants, i.e. at apply time — not in `afterEvaluate`. The
        // plugin list itself is resolved lazily inside the callback so
        // the `istmo { … }` extension is fully configured by then.
        val linked: Lazy<List<LinkedPlugin>> = lazy { resolveLinkedPlugins(project, extension) }
        project.pluginManager.withPlugin("com.android.application") {
            val components = project.extensions.findByType(AndroidComponentsExtension::class.java)
            if (components == null) {
                project.logger.warn("istmo-plugin-loader: androidComponents extension not found; manifest/res merging disabled.")
                return@withPlugin
            }
            components.onVariants(components.selector().all()) { variant ->
                wireVariant(project, variant, linked.value)
            }
        }

        project.afterEvaluate {
            val root = resolveWorkspaceRoot(project, extension)
            if (root == null) {
                project.logger.warn(
                    "istmo-plugin-loader: no Cargo workspace root above ${project.rootDir}; skipping auto-linking.",
                )
                return@afterEvaluate
            }

            val android = project.extensions.findByType(BaseExtension::class.java)
            if (android == null) {
                project.logger.warn(
                    "istmo-plugin-loader: no Android Gradle extension on ${project.path}; apply after com.android.application.",
                )
                return@afterEvaluate
            }

            val consumerCargo = consumerCargoToml(project)
            if (consumerCargo == null) {
                project.logger.warn(
                    "istmo-plugin-loader: no Cargo.toml next to ${project.rootDir}; skipping.",
                )
                return@afterEvaluate
            }
            val consumerDeps = readConsumerDeps(consumerCargo)
            val dirs = discoverPlugins(root, extension, consumerDeps).map { it.nativeDir }
            if (dirs.isEmpty()) {
                project.logger.info("istmo-plugin-loader: no plugin native/android directories to link.")
                return@afterEvaluate
            }

            val mainSourceSet = android.sourceSets.getByName("main")
            val existing = mainSourceSet.java.srcDirs.toMutableSet()
            existing.addAll(dirs)
            mainSourceSet.java.setSrcDirs(existing)

            val names = dirs.joinToString(", ") { it.parentFile.name }
            project.logger.lifecycle(
                "istmo-plugin-loader: linked ${dirs.size} plugin source dir(s) [$names]",
            )
        }
    }

    private fun resolveLinkedPlugins(project: Project, ext: IstmoLoaderExtension): List<LinkedPlugin> {
        val root = resolveWorkspaceRoot(project, ext) ?: return emptyList()
        val consumerCargo = consumerCargoToml(project) ?: return emptyList()
        return discoverPlugins(root, ext, readConsumerDeps(consumerCargo))
    }

    private fun wireVariant(project: Project, variant: Variant, plugins: List<LinkedPlugin>) {
        if (plugins.isEmpty()) return
        checkMinSdk(variant, plugins)
        for (plugin in plugins) {
            plugin.manifest?.let { manifest ->
                try {
                    variant.sources.manifests.addStaticManifestFile(manifest.absolutePath)
                } catch (_: LinkageError) {
                    // `Sources.manifests` landed in AGP 8.3. Older AGPs
                    // keep working; the author merges by hand.
                    project.logger.warn(
                        "istmo-plugin-loader: AGP < 8.3 cannot merge ${manifest.path}; " +
                            "copy its <application> children into your AndroidManifest.xml or upgrade AGP.",
                    )
                }
            }
            plugin.resDir?.let { res ->
                variant.sources.res?.addStaticSourceDirectory(res.absolutePath)
            }
        }
    }

    private fun checkMinSdk(variant: Variant, plugins: List<LinkedPlugin>) {
        val minSdk = try {
            variant.minSdk.apiLevel
        } catch (_: LinkageError) {
            // `Variant.minSdk` replaced `minSdkVersion` in AGP 8.1.
            @Suppress("DEPRECATION")
            variant.minSdkVersion.apiLevel
        }
        val violations = plugins.filter { (it.minAndroidApi ?: 0) > minSdk }
        if (violations.isEmpty()) return
        val report = violations.joinToString("\n") {
            "  - ${it.crateName} requires minSdk >= ${it.minAndroidApi}"
        }
        throw GradleException(
            "istmo-plugin-loader: variant '${variant.name}' has minSdk $minSdk, but:\n$report\n" +
                "Raise minSdk or drop the plugin.",
        )
    }

    private fun resolveWorkspaceRoot(project: Project, ext: IstmoLoaderExtension): File? {
        val override = ext.workspaceRoot.orNull
        if (override != null) return override
        var cur: File? = project.rootDir
        while (cur != null) {
            val cargo = File(cur, "Cargo.toml")
            if (cargo.isFile) {
                val toml = try {
                    Toml.parse(cargo.toPath())
                } catch (_: Exception) {
                    null
                }
                if (toml != null && toml.contains("workspace")) return cur
            }
            cur = cur.parentFile
        }
        return null
    }

    private fun discoverPlugins(
        workspaceRoot: File,
        ext: IstmoLoaderExtension,
        consumerDeps: Set<String>,
    ): List<LinkedPlugin> {
        val cargoToml = File(workspaceRoot, "Cargo.toml")
        val toml = try {
            Toml.parse(cargoToml.toPath())
        } catch (_: Exception) {
            return emptyList()
        }
        val members: List<String> = toml.getArray("workspace.members")
            ?.let { arr ->
                val out = mutableListOf<String>()
                for (i in 0 until arr.size()) {
                    val s = arr.getString(i)
                    if (s != null) out.add(s)
                }
                out
            }
            ?: emptyList()

        val exclude: Set<String> = ext.excludePlugins.get().toSet()
        val expanded = mutableListOf<File>()
        for (pattern in members) {
            expanded.addAll(expandGlob(workspaceRoot, pattern))
        }

        val result = mutableListOf<LinkedPlugin>()
        for (crate in expanded.distinct()) {
            val crateName = readCrateName(crate) ?: continue
            if (exclude.contains(crateName)) continue
            // Only inject plugins the consumer actually depends on.
            // Otherwise every workspace plugin's native/android/ would
            // land in every app's source set, dragging in Kotlin files
            // that reference codegen output that only exists when the
            // matching contract handover is active.
            if (crateName !in consumerDeps) continue
            val istmoToml = File(crate, "istmo.toml")
            if (!istmoToml.isFile) continue
            val nativeDir = File(crate, "native/android")
            if (!nativeDir.isDirectory) continue
            result.add(
                LinkedPlugin(
                    crateName = crateName,
                    nativeDir = nativeDir,
                    manifest = File(nativeDir, "AndroidManifest.xml").takeIf { it.isFile },
                    resDir = File(nativeDir, "res").takeIf { it.isDirectory },
                    minAndroidApi = readMinAndroidApi(istmoToml),
                ),
            )
        }
        return result
    }

    private fun readMinAndroidApi(istmoToml: File): Int? {
        val toml = try {
            Toml.parse(istmoToml.toPath())
        } catch (_: Exception) {
            return null
        }
        return toml.getLong("min_versions.android")?.toInt()
    }

    private fun readCrateName(crateDir: File): String? {
        val cargo = File(crateDir, "Cargo.toml")
        if (!cargo.isFile) return null
        val toml = try {
            Toml.parse(cargo.toPath())
        } catch (_: Exception) {
            return null
        }
        return toml.getString("package.name")
    }

    private fun readConsumerDeps(consumerCargoToml: File): Set<String> {
        val toml = try {
            Toml.parse(consumerCargoToml.toPath())
        } catch (_: Exception) {
            return emptySet()
        }
        val result = mutableSetOf<String>()
        // Top-level `[dependencies]` + `[dev-dependencies]`.
        // Target-scoped deps under `[target.'cfg(...)']` are
        // deliberately skipped — plugins that ship
        // `native/android/` are conventionally declared as
        // unconditional dependencies (the target cfg lives in the
        // plugin's own Cargo.toml, not the consumer's). Adding
        // target-scoped parsing here means walking key names that
        // include `(` / `,` / `"` which tomlj rejects as dotted-key
        // paths.
        toml.getTable("dependencies")?.keySet()?.forEach { result.add(it) }
        toml.getTable("dev-dependencies")?.keySet()?.forEach { result.add(it) }
        return result
    }

    /**
     * Locate the Cargo crate that lives next to the Gradle project —
     * an istmo app conventionally has `pen-demo/Cargo.toml` alongside
     * `pen-demo/android/`. The plugin loader restricts its injection
     * to plugins listed as dependencies of that crate.
     */
    private fun consumerCargoToml(project: Project): File? {
        val parent = project.rootDir.parentFile ?: return null
        val cargo = File(parent, "Cargo.toml")
        return if (cargo.isFile) cargo else null
    }

    private fun expandGlob(workspaceRoot: File, pattern: String): List<File> {
        if (!pattern.contains('*')) {
            val dir = File(workspaceRoot, pattern)
            return if (dir.isDirectory) listOf(dir) else emptyList()
        }
        val parts = pattern.split('/')
        var candidates: List<File> = listOf(workspaceRoot)
        for (segment in parts) {
            candidates = if (segment == "*") {
                candidates.flatMap { parent ->
                    val list = parent.listFiles() ?: emptyArray()
                    list.filter { it.isDirectory }
                }
            } else {
                candidates.map { File(it, segment) }.filter { it.exists() }
            }
        }
        return candidates.filter { it.isDirectory }
    }
}
