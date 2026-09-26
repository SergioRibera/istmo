package dev.istmo.gradle

import com.android.build.api.AndroidPluginVersion
import com.android.build.api.variant.AndroidComponentsExtension
import com.android.build.gradle.BaseExtension
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
 * Auto-linking Gradle plugin for istmo apps. Walks upward from the
 * consumer app's `project.rootDir` until it finds a Cargo workspace
 * root (a `Cargo.toml` with a `[workspace]` table), enumerates the
 * workspace's plugin crates by looking at each member's `istmo.toml`
 * + `native/android/` sibling, and:
 *
 * - adds every existing directory to the Android app's `main` Kotlin
 *   source set;
 * - merges each plugin's `native/android/AndroidManifest.xml` into
 *   every variant's manifest — through the Variant API on AGP 8.3+, or,
 *   on older AGPs, by combining them into one overlay registered as
 *   each build type's manifest;
 * - adds each plugin's `native/android/res/` as a resource directory;
 * - fails the build when a plugin's `[min_versions] android` is newer
 *   than the variant's `minSdk`.
 *
 * No files are copied.
 *
 * Superseded by `dev.istmo.app`, which also builds the Rust library,
 * links plugins from crates.io / git (not just workspace members) and
 * applies `[app]` from `istmo.toml`.
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
        var variantManifests = false
        project.pluginManager.withPlugin("com.android.application") {
            val components = project.extensions.findByType(AndroidComponentsExtension::class.java)
            if (components == null) {
                project.logger.warn("istmo-plugin-loader: androidComponents extension not found; minSdk checks disabled.")
                return@withPlugin
            }
            // `Sources.manifests` landed in AGP 8.3; older AGPs get the
            // build-type overlay wired in `afterEvaluate` below.
            variantManifests = components.pluginVersion >= AndroidPluginVersion(8, 3)
            components.onVariants(components.selector().all()) { variant ->
                PluginLinking.wireVariant(variant, linked.value, variantManifests, "istmo-plugin-loader")
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
            val plugins = linked.value
            val dirs = plugins.map { it.nativeDir }
            if (dirs.isEmpty()) {
                project.logger.info("istmo-plugin-loader: no plugin native/android directories to link.")
                return@afterEvaluate
            }

            val mainSourceSet = android.sourceSets.getByName("main")
            val existing = mainSourceSet.java.srcDirs.toMutableSet()
            existing.addAll(dirs)
            mainSourceSet.java.setSrcDirs(existing)

            // `res/` through the classic source-set DSL: works on every AGP.
            plugins.mapNotNull { it.resDir }.forEach { mainSourceSet.res.srcDir(it) }

            if (!variantManifests) {
                wireLegacyManifestOverlay(project, android, plugins)
            }

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

    /**
     * AGP < 8.3: combine every plugin manifest into one overlay and
     * register it as the manifest of each build type whose source set
     * has none. Build-type manifests are merged above `main`, exactly
     * like `addStaticManifestFile` overlays on newer AGPs.
     */
    private fun wireLegacyManifestOverlay(project: Project, android: BaseExtension, plugins: List<LinkedPlugin>) {
        val manifests = plugins.mapNotNull { it.manifest }
        if (manifests.isEmpty()) return
        val overlay = project.layout.buildDirectory.file("istmo/plugin-manifests/AndroidManifest.xml").get().asFile
        PluginLinking.writeCombinedManifest(manifests, overlay)
        for (buildType in android.buildTypes) {
            val sourceSet = android.sourceSets.getByName(buildType.name)
            val own = sourceSet.manifest.srcFile
            if (own.isFile && own.canonicalFile != overlay.canonicalFile) {
                project.logger.warn(
                    "istmo-plugin-loader: build type '${buildType.name}' has its own ${own.path}; " +
                        "merge ${overlay.path} into it by hand or upgrade to AGP 8.3+.",
                )
                continue
            }
            sourceSet.manifest.srcFile(overlay)
        }
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
