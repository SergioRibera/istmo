package dev.istmo.gradle

import com.android.build.gradle.BaseExtension
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.api.provider.Property
import org.tomlj.Toml
import org.tomlj.TomlParseResult
import java.io.File

/**
 * Auto-linking Gradle plugin for istmo apps.
 *
 * Walks upward from the consumer app's `project.rootDir` until it finds a
 * Cargo workspace root (a `Cargo.toml` with a `[workspace]` table),
 * enumerates the workspace's plugin crates by looking at each member's
 * `istmo.toml` + `native/android/` sibling, and injects every existing
 * directory into the Android app's `main` Kotlin source set. No files
 * are copied — the plugin's own source tree becomes the compile input.
 *
 * The equivalent for iOS is provided by `istmo-build`'s `emit_app`, which
 * writes an `ios/istmo-plugins.yml` fragment for xcodegen consumers to
 * include; a native Swift Package auto-linker is a future milestone.
 */
class IstmoLoaderPlugin : Plugin<Project> {
    override fun apply(project: Project) {
        val extension = project.extensions.create(
            "istmo",
            IstmoLoaderExtension::class.java,
            project,
        )

        project.afterEvaluate { p ->
            val workspaceRoot = resolveWorkspaceRoot(p, extension)
                ?: run {
                    p.logger.warn(
                        "istmo-plugin-loader: no Cargo workspace root found above ${p.rootDir}; skipping auto-linking.",
                    )
                    return@afterEvaluate
                }

            val android = p.extensions.findByType(BaseExtension::class.java)
                ?: run {
                    p.logger.warn(
                        "istmo-plugin-loader: no Android Gradle extension on ${p.path}; apply after com.android.application / com.android.library.",
                    )
                    return@afterEvaluate
                }

            val dirs = discoverPluginNativeAndroidDirs(workspaceRoot, extension)
            if (dirs.isEmpty()) {
                p.logger.info("istmo-plugin-loader: no plugin native/android directories to link.")
                return@afterEvaluate
            }

            val main = android.sourceSets.getByName("main")
            val existing = main.java.srcDirs.toMutableSet()
            existing.addAll(dirs)
            main.java.setSrcDirs(existing)

            p.logger.lifecycle(
                "istmo-plugin-loader: linked ${dirs.size} plugin kotlin source dir(s) — ${dirs.joinToString { it.name }}",
            )
        }
    }

    private fun resolveWorkspaceRoot(project: Project, ext: IstmoLoaderExtension): File? {
        ext.workspaceRoot.orNull?.let { return it }
        var cur: File? = project.rootDir
        while (cur != null) {
            val cargo = cur.resolve("Cargo.toml")
            if (cargo.isFile) {
                val toml = runCatching { Toml.parse(cargo.toPath()) }.getOrNull()
                if (toml != null && toml.contains("workspace")) return cur
            }
            cur = cur.parentFile
        }
        return null
    }

    private fun discoverPluginNativeAndroidDirs(
        workspaceRoot: File,
        ext: IstmoLoaderExtension,
    ): List<File> {
        val toml = runCatching { Toml.parse(workspaceRoot.resolve("Cargo.toml").toPath()) }
            .getOrNull() ?: return emptyList()
        val members = toml.getArray("workspace.members")?.toList().orEmpty()
            .filterIsInstance<String>()

        val exclude = ext.excludePlugins.get().toSet()

        return members.mapNotNull { pattern ->
            resolveGlob(workspaceRoot, pattern)
        }
            .flatten()
            .distinct()
            .filter { crateDir -> crateDir.resolve("istmo.toml").isFile }
            .filterNot { crateDir -> exclude.contains(crateDir.name) }
            .map { it.resolve("native/android") }
            .filter { it.isDirectory }
    }

    /**
     * Cargo workspace members may include glob patterns like
     * `plugins/*` — expand them into concrete directories relative to
     * the workspace root.
     */
    private fun resolveGlob(workspaceRoot: File, pattern: String): List<File>? {
        if (!pattern.contains('*')) {
            val dir = workspaceRoot.resolve(pattern)
            return if (dir.isDirectory) listOf(dir) else null
        }
        val parts = pattern.split('/')
        var candidates: List<File> = listOf(workspaceRoot)
        for (segment in parts) {
            candidates = if (segment == "*") {
                candidates.flatMap { parent ->
                    parent.listFiles { f -> f.isDirectory }?.toList().orEmpty()
                }
            } else {
                candidates.map { it.resolve(segment) }.filter { it.exists() }
            }
        }
        return candidates.filter { it.isDirectory }
    }
}

open class IstmoLoaderExtension(project: Project) {

    /**
     * Cargo workspace root override. Leave `null` (the default) to
     * search upward from the Gradle project's `rootDir` for a
     * `Cargo.toml` declaring `[workspace]`.
     */
    val workspaceRoot: Property<File> = project.objects.property(File::class.java)

    /**
     * Skip these workspace member directory names when auto-linking —
     * useful when a crate ships `native/android/` but the app wants to
     * bring its own implementation of that plugin.
     */
    val excludePlugins: org.gradle.api.provider.ListProperty<String> =
        project.objects.listProperty(String::class.java).convention(emptyList())
}
