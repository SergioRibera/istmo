import org.gradle.api.tasks.Exec

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.istmo.multi"
    compileSdk = 34

    defaultConfig {
        applicationId = "dev.istmo.multi"
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

// Route the Gradle-managed rustJniLibs/ under $buildDir into the standard
// JNI merge and REPLACE the default `src/main/jniLibs/` source dir — build
// artefacts never live under src/.
android.sourceSets["main"].jniLibs.setSrcDirs(
    listOf(layout.buildDirectory.dir("rustJniLibs").get().asFile),
)

// ---- istmo cargo integration ---------------------------------------
//
// Convention-based glue: Gradle IS the build root and drives cargo per
// (crate, ABI). Multiple `istmoCargoLib(...)` calls stack — every resulting
// `.so` lands in `$buildDir/rustJniLibs/<abi>/` and joins the JNI merge.
// This is what the multi-cdylib packaging model needs.
//
// Rejected alternatives are documented in the sister demo
// (`examples/android-demo/android/app/build.gradle.kts`).

val abiToRustTarget = mapOf(
    "arm64-v8a" to "aarch64-linux-android",
    "armeabi-v7a" to "armv7-linux-androideabi",
    "x86_64" to "x86_64-linux-android",
    "x86" to "i686-linux-android",
)

// Workspace root: project.rootDir is the android/ Gradle root (settings.
// gradle.kts lives there); climb three levels — android/ → android-multi/
// → examples/ → istmo/ (the cargo workspace root).
val workspaceRoot: File = project.rootDir.resolve("../../..").normalize()
val rustJniLibsDir = layout.buildDirectory.dir("rustJniLibs")

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

afterEvaluate {
    tasks.matching { it.name.matches(Regex("merge.*JniLibFolders")) }.configureEach {
        cargoStageTaskNames.forEach { dependsOn(it) }
    }
}

// ---- Declare the demo's cdylibs -------------------------------------
// Two calls proves the multi-cdylib packaging path: both `.so` files land
// in the same rustJniLibs/<abi>/ folder and merge into a single APK. Only
// the app cdylib is loaded at runtime today; the widget cdylib is a
// packaging scaffold for the future widget-process artifact.
istmoCargoLib("istmo-android-multi-app")
istmoCargoLib("istmo-android-multi-widget")

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.lifecycle:lifecycle-service:2.8.4")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
}
