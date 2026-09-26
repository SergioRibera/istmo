package dev.istmo.gradle

import groovy.json.JsonSlurper
import org.gradle.api.DefaultTask
import org.gradle.api.GradleException
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.file.RegularFileProperty
import org.gradle.api.provider.ListProperty
import org.gradle.api.provider.MapProperty
import org.gradle.api.provider.Property
import org.gradle.api.tasks.Input
import org.gradle.api.tasks.Internal
import org.gradle.api.tasks.OutputDirectory
import org.gradle.api.tasks.TaskAction
import java.io.File

/**
 * `cargo build --target <triple> --profile <profile>` for the app crate,
 * staging the resulting `lib<name>.so` into [outputDir] (one ABI
 * directory of the build type's `jniLibs`).
 *
 * Always runs: cargo does its own incremental work, and Gradle cannot
 * see the full input set (every crate the app depends on).
 */
abstract class CargoBuildTask : DefaultTask() {
    @get:Input abstract val cargo: Property<String>
    @get:Input abstract val manifestPath: Property<String>
    @get:Input abstract val rustTarget: Property<String>
    @get:Input abstract val profile: Property<String>
    @get:Input abstract val extraArgs: ListProperty<String>
    @get:Input abstract val environment: MapProperty<String, String>
    @get:OutputDirectory abstract val outputDir: DirectoryProperty

    /** `android/.istmo/istmo.json`, re-read after cargo runs. */
    @get:Internal abstract val metadataFile: RegularFileProperty

    /** Metadata contents Gradle was configured with. */
    @get:Internal abstract val metadataSnapshot: Property<String>

    init {
        group = "istmo"
        outputs.upToDateWhen { false }
    }

    @TaskAction
    fun build() {
        val manifest = File(manifestPath.get()).canonicalFile
        val command = listOf(
            cargo.get(), "build",
            "--manifest-path", manifest.path,
            "--lib",
            "--target", rustTarget.get(),
            "--profile", profile.get(),
            "--message-format=json-render-diagnostics",
        ) + extraArgs.get()
        logger.lifecycle("istmo: cargo build --target ${rustTarget.get()} --profile ${profile.get()}")

        val process = ProcessBuilder(command).apply { environment().putAll(this@CargoBuildTask.environment.get()) }.start()
        val stderrTail = ArrayDeque<String>()
        val errPump = Thread {
            process.errorStream.bufferedReader().forEachLine { line ->
                when {
                    line.startsWith("error") -> logger.error(line)
                    line.startsWith("warning") -> logger.warn(line)
                    else -> logger.info(line)
                }
                synchronized(stderrTail) {
                    stderrTail.addLast(line)
                    if (stderrTail.size > STDERR_TAIL_LINES) stderrTail.removeFirst()
                }
            }
        }.apply { start() }
        val libraries = mutableListOf<File>()
        process.inputStream.bufferedReader().forEachLine { line ->
            libraries += cdylibsIn(line, manifest)
        }
        val exitCode = process.waitFor()
        errPump.join()
        if (exitCode != 0) {
            val tail = synchronized(stderrTail) { stderrTail.joinToString("\n") }
            throw GradleException("istmo: cargo build --target ${rustTarget.get()} failed (exit $exitCode):\n$tail")
        }

        val library = libraries.lastOrNull()
            ?: throw GradleException(
                "istmo: cargo built no shared library for ${manifest.path}; add \"cdylib\" to `[lib] crate-type`.",
            )
        val dest = outputDir.get().asFile
        dest.mkdirs()
        library.copyTo(File(dest, library.name), overwrite = true)

        checkMetadataUnchanged()
    }

    /** `.so` files of the `cdylib` built for [manifest], from one cargo JSON message. */
    private fun cdylibsIn(line: String, manifest: File): List<File> {
        if (!line.startsWith("{") || !line.contains("\"compiler-artifact\"")) return emptyList()
        val message = JsonSlurper().parseText(line) as? Map<*, *> ?: return emptyList()
        val artifactManifest = (message["manifest_path"] as? String)?.let { File(it).canonicalFile }
        val kinds = (message["target"] as? Map<*, *>)?.get("kind") as? List<*> ?: return emptyList()
        if (artifactManifest != manifest || "cdylib" !in kinds) return emptyList()
        return (message["filenames"] as? List<*>).orEmpty()
            .mapNotNull { it as? String }
            .filter { it.endsWith(".so") }
            .map(::File)
    }

    /**
     * Cargo may have re-run the app's build script with a different plugin
     * set (a dependency was added or removed). Gradle already configured
     * source sets, manifests and dependencies from the old one, so the
     * build must run again.
     */
    private fun checkMetadataUnchanged() {
        val file = metadataFile.get().asFile
        if (!file.isFile) return
        if (file.readText() != metadataSnapshot.get()) {
            throw GradleException(
                "istmo: the app's plugins or [app] settings changed while cargo ran (${file.path}). " +
                    "Run the build again so Gradle picks them up.",
            )
        }
    }

    private companion object {
        const val STDERR_TAIL_LINES = 200
    }
}
