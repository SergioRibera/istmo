package dev.istmo.gradle

import org.gradle.api.GradleException
import org.gradle.api.Project
import java.io.File
import java.util.concurrent.TimeUnit

/** One `istmoDoctor` line. [fix] says what to do when [ok] is false. */
data class DoctorCheck(val ok: Boolean, val label: String, val fix: String? = null)

/** Captured result of a cargo / rustup invocation. */
data class ProcessResult(val exitCode: Int, val stdout: String, val stderr: String)

/**
 * The Rust + NDK toolchain the Android build drives: finds `cargo` and
 * `rustup` even when Android Studio was started without the shell `PATH`,
 * points cargo at the NDK's clang for linking and for `cc`-based build
 * scripts, and reports what is missing with the command that fixes it
 * (the `istmoDoctor` task).
 */
internal class Toolchain(
    val project: Project,
    private val cargoOverride: () -> String?,
    private val ndkLocator: () -> File?,
) {
    private val isWindows = System.getProperty("os.name").lowercase().contains("windows")
    private val exe = if (isWindows) ".exe" else ""

    val cargo: File? by lazy { cargoOverride()?.let(::File) ?: findTool("cargo") }
    val rustup: File? by lazy { findTool("rustup") }

    private val installedTargets: Set<String>? by lazy {
        val rustup = rustup ?: return@lazy null
        val result = run(listOf(rustup.path, "target", "list", "--installed"), emptyMap())
        if (result.exitCode == 0) result.stdout.lines().map { it.trim() }.filter { it.isNotEmpty() }.toSet() else null
    }

    private val ndkDir: File? by lazy {
        runCatching(ndkLocator).getOrNull()?.takeIf { it.isDirectory }
            ?: listOf("ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "ANDROID_NDK")
                .firstNotNullOfOrNull { System.getenv(it)?.let(::File)?.takeIf(File::isDirectory) }
            ?: newestSdkNdk()
    }

    /** Newest `<sdk>/ndk/<version>` of the SDK named by `ANDROID_HOME` / `ANDROID_SDK_ROOT`. */
    private fun newestSdkNdk(): File? =
        listOf("ANDROID_HOME", "ANDROID_SDK_ROOT")
            .firstNotNullOfOrNull { System.getenv(it)?.let(::File)?.takeIf(File::isDirectory) }
            ?.resolve("ndk")
            ?.listFiles { file -> file.isDirectory }
            ?.maxByOrNull { dir -> dir.name.split('.').map { it.toIntOrNull() ?: 0 }.fold(0L) { acc, n -> acc * 100_000 + n } }

    /** `<ndk>/toolchains/llvm/prebuilt/<host>/bin`. */
    private val ndkBin: File? by lazy {
        val prebuilt = ndkDir?.resolve("toolchains/llvm/prebuilt") ?: return@lazy null
        val host = when {
            isWindows -> "windows-x86_64"
            System.getProperty("os.name").lowercase().contains("mac") -> "darwin-x86_64"
            else -> "linux-x86_64"
        }
        (prebuilt.resolve(host).takeIf { it.isDirectory } ?: prebuilt.listFiles()?.firstOrNull { it.isDirectory })
            ?.resolve("bin")
            ?.takeIf { it.isDirectory }
    }

    fun checks(rustTargets: List<String>): List<DoctorCheck> {
        val checks = mutableListOf<DoctorCheck>()
        val cargo = cargo
        checks += if (cargo != null) {
            DoctorCheck(true, "cargo: ${cargo.path}")
        } else {
            DoctorCheck(false, "cargo not found", "install Rust from https://rustup.rs or set `istmo { cargo = \"/path/to/cargo\" }`")
        }
        val installed = installedTargets
        for (target in rustTargets) {
            checks += when {
                installed == null -> DoctorCheck(true, "Rust target $target (rustup not found; not verified)")
                target in installed -> DoctorCheck(true, "Rust target $target")
                else -> DoctorCheck(false, "Rust target $target is not installed", "run `rustup target add $target`")
            }
        }
        for (target in rustTargets) {
            val linkerVar = linkerVar(target)
            checks += when {
                System.getenv(linkerVar) != null -> DoctorCheck(true, "linker for $target: \$$linkerVar")
                ndkBin != null -> DoctorCheck(true, "linker for $target: NDK ${ndkDir!!.name}")
                else -> DoctorCheck(
                    false,
                    "no Android NDK found to link $target",
                    "install one with `sdkmanager \"ndk;<version>\"` (or Android Studio's SDK Manager) and set " +
                        "`android.ndkVersion`, or export ANDROID_NDK_HOME",
                )
            }
        }
        return checks
    }

    /** Fail the build with every missing piece and its fix. */
    fun requireReady(rustTargets: List<String>) {
        val failures = checks(rustTargets).filterNot { it.ok }
        if (failures.isEmpty()) return
        val report = failures.joinToString("\n") { "  ✗ ${it.label}: ${it.fix}" }
        throw GradleException("istmo: the Rust toolchain is not ready:\n$report\nRun `./gradlew istmoDoctor` for the full report.")
    }

    /**
     * Environment for `cargo … --target [rustTarget]`: cargo's directory on
     * `PATH`, plus NDK linker / `CC` / `CXX` / `AR` for the target unless
     * the user already exported them.
     */
    fun cargoEnvironment(rustTarget: String, minSdk: Int): Map<String, String> {
        val env = linkedMapOf<String, String>()
        cargo?.parentFile?.let { dir ->
            env["PATH"] = dir.path + File.pathSeparator + (System.getenv("PATH") ?: "")
        }
        val bin = ndkBin ?: return env
        val api = maxOf(minSdk, MIN_NDK_API)
        val clangTriple = CLANG_TRIPLES[rustTarget] ?: return env
        val script = if (isWindows) ".cmd" else ""
        val underscored = rustTarget.replace('-', '_')
        fun putIfUnset(key: String, value: File) {
            if (System.getenv(key) == null && value.exists()) env[key] = value.path
        }
        putIfUnset(linkerVar(rustTarget), bin.resolve("$clangTriple$api-clang$script"))
        putIfUnset("CC_$underscored", bin.resolve("$clangTriple$api-clang$script"))
        putIfUnset("CXX_$underscored", bin.resolve("$clangTriple$api-clang++$script"))
        putIfUnset("AR_$underscored", bin.resolve("llvm-ar$exe"))
        return env
    }

    fun runCargo(args: List<String>, env: Map<String, String>): ProcessResult {
        val cargo = cargo ?: throw GradleException("istmo: cargo not found; run `./gradlew istmoDoctor`")
        return run(listOf(cargo.path) + args, env)
    }

    private fun run(command: List<String>, env: Map<String, String>): ProcessResult {
        val process = ProcessBuilder(command).apply { environment().putAll(env) }.start()
        val stderr = StringBuilder()
        val errPump = Thread { stderr.append(process.errorStream.bufferedReader().readText()) }.apply { start() }
        val stdout = process.inputStream.bufferedReader().readText()
        process.waitFor(1, TimeUnit.HOURS)
        errPump.join()
        return ProcessResult(process.exitValue(), stdout, stderr.toString())
    }

    private fun findTool(name: String): File? {
        val fromPath = System.getenv("PATH")
            ?.split(File.pathSeparator)
            ?.map { File(it, name + exe) }
            ?.firstOrNull { it.canExecute() }
        if (fromPath != null) return fromPath
        val cargoHome = System.getenv("CARGO_HOME")?.let(::File)
            ?: File(System.getProperty("user.home"), ".cargo")
        return File(cargoHome, "bin/$name$exe").takeIf { it.canExecute() }
    }

    private fun linkerVar(rustTarget: String) =
        "CARGO_TARGET_${rustTarget.uppercase().replace('-', '_')}_LINKER"

    companion object {
        /** Oldest API level current NDKs ship clang wrappers for. */
        private const val MIN_NDK_API = 21

        val ABI_TO_RUST_TARGET = mapOf(
            "arm64-v8a" to "aarch64-linux-android",
            "armeabi-v7a" to "armv7-linux-androideabi",
            "x86_64" to "x86_64-linux-android",
            "x86" to "i686-linux-android",
        )

        private val CLANG_TRIPLES = mapOf(
            "aarch64-linux-android" to "aarch64-linux-android",
            "armv7-linux-androideabi" to "armv7a-linux-androideabi",
            "x86_64-linux-android" to "x86_64-linux-android",
            "i686-linux-android" to "i686-linux-android",
        )
    }
}
