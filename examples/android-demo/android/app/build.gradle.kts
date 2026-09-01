import org.gradle.api.tasks.Exec

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.istmo.demo"
    compileSdk = 34

    defaultConfig {
        applicationId = "dev.istmo.demo"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1"
        ndk {
            // SINGLE POINT OF CHANGE for target architectures. istmoCargoLib
            // reads this list and registers a matching cargo task per
            // (crate, ABI). Adding an ABI = add it here.
            abiFilters += setOf("arm64-v8a")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    packaging {
        jniLibs {
            useLegacyPackaging = false
        }
    }
}

// Route the Gradle-managed rustJniLibs/ under $buildDir into the
// standard JNI merge. Build artefacts NEVER live under src/. Pulled out
// of the `android { }` block so `layout` resolves at Project scope.
android.sourceSets["main"].jniLibs.srcDir(layout.buildDirectory.dir("rustJniLibs"))

// ---- istmo cargo integration ---------------------------------------
//
// Manual recipe for this demo; will be promoted to `istmo.gradle.kts`
// (emitted by istmo-build + shipped in project-template) once the
// contract is stable. Design: Gradle IS the build root and drives
// cargo per (crate, ABI). Never the inverse.
//
// The rejected alternatives:
// * `org.mozilla.rust-android-gradle` — assumes exactly one cargo module
//   per Gradle project, incompatible with the multi-cdylib workspace
//   model (app + service + widget produce distinct .so files that all
//   land in one APK).
// * `cargo-ndk` — the docker image already presets NDK toolchain env
//   vars for every android target; the extra dependency buys nothing.

val abiToRustTarget = mapOf(
    "arm64-v8a" to "aarch64-linux-android",
    "armeabi-v7a" to "armv7-linux-androideabi",
    "x86_64" to "x86_64-linux-android",
    "x86" to "i686-linux-android",
)

// Workspace root: project.rootDir is the android/ Gradle root; climb
// three levels to reach istmo/, the cargo workspace root.
val workspaceRoot: File = project.rootDir.resolve("../../..").normalize()
val rustJniLibsDir = layout.buildDirectory.dir("rustJniLibs")

// Shared inputs. Coarse but correct: cover every workspace member so
// Gradle's up-to-date check fires when any istmo-* crate changes, not
// just when Cargo.lock moves. Cargo does the fine-grained incremental
// work; Gradle just decides whether to invoke it.
val workspaceSources: FileTree = fileTree(workspaceRoot) {
    include(
        "crates/**/src/**",
        "crates/**/Cargo.toml",
        "crates/**/build.rs",
        "examples/**/src/**",
        "examples/**/Cargo.toml",
        "examples/**/build.rs",
        "src/**",
        "Cargo.toml",
        "Cargo.lock",
    )
    exclude(
        "target/**",
        "examples/*/android/**",
    )
}

val cargoStageTaskNames = mutableListOf<String>()

/**
 * Register per-ABI cargo build + staging tasks for a workspace crate.
 *
 * @param crate    workspace package name, e.g. `"istmo-android-demo"`.
 * @param libName  cdylib name (defaults to `crate.replace('-', '_')`,
 *                 matching what cargo emits as `lib<libName>.so`).
 *
 * Multi-cdylib support: call this once per artifact. All .so files land
 * in `$buildDir/rustJniLibs/<abi>/` and are picked up together by AGP's
 * JNI merge.
 */
fun istmoCargoLib(crate: String, libName: String = crate.replace('-', '_')) {
    val soName = "lib$libName.so"
    val abis = android.defaultConfig.ndk.abiFilters
    require(abis.isNotEmpty()) {
        "abiFilters must be set (in android.defaultConfig.ndk) before calling istmoCargoLib"
    }

    for (abi in abis) {
        val rustTarget = abiToRustTarget[abi]
            ?: error("no rust target mapping for ABI '$abi'")
        val suffix = "${libName}_${abi.replace('-', '_')}"
        val cargoSo = workspaceRoot.resolve("target/$rustTarget/release/$soName")
        val stagedSo = rustJniLibsDir.map { it.dir(abi).file(soName) }

        val cargoTask = tasks.register("cargoBuild_$suffix", Exec::class) {
            group = "istmo"
            description = "cargo build --release --target $rustTarget -p $crate"
            workingDir = workspaceRoot
            commandLine(
                "cargo", "build", "--release",
                "--target", rustTarget,
                "-p", crate,
            )
            inputs.files(workspaceSources).withPropertyName("workspaceSources")
            outputs.file(cargoSo).withPropertyName("cargoSo")
        }

        val stageTask = tasks.register("stageRustLib_$suffix") {
            group = "istmo"
            description = "stage $soName into rustJniLibs/$abi"
            dependsOn(cargoTask)
            inputs.file(cargoSo)
            outputs.file(stagedSo)
            doLast {
                val dst = stagedSo.get().asFile
                dst.parentFile.mkdirs()
                cargoSo.copyTo(dst, overwrite = true)
            }
        }
        cargoStageTaskNames.add(stageTask.name)
    }
}

// Hook the staging tasks into every JNI merge task. `afterEvaluate` runs
// once AGP has produced the per-variant task graph.
afterEvaluate {
    tasks.matching { it.name.matches(Regex("merge.*JniLibFolders")) }.configureEach {
        cargoStageTaskNames.forEach { dependsOn(it) }
    }
}

// ---- Declare the demo's cdylibs -------------------------------------
istmoCargoLib("istmo-android-demo")

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
}
