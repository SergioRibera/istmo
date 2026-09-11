import org.gradle.api.tasks.Exec

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.istmo.datastoredemo"
    compileSdk = 34

    defaultConfig {
        applicationId = "dev.istmo.datastoredemo"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1"
        ndk {
            abiFilters += setOf("arm64-v8a")
        }
    }

    // Pin the debug signing config so it always resolves to the
    // developer's `~/.android/debug.keystore` — inside Docker the path is
    // bind-mounted from the host.
    signingConfigs {
        getByName("debug") {
            storeFile = file(
                System.getenv("ANDROID_DEBUG_KEYSTORE")
                    ?: "${System.getProperty("user.home")}/.android/debug.keystore",
            )
            storePassword = "android"
            keyAlias = "androiddebugkey"
            keyPassword = "android"
        }
    }

    buildTypes {
        getByName("debug") {
            signingConfig = signingConfigs.getByName("debug")
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

android.sourceSets["main"].jniLibs.setSrcDirs(
    listOf(layout.buildDirectory.dir("rustJniLibs").get().asFile),
)

// ---- istmo cargo integration ----------------------------------------
//
// Gradle drives cargo. The cdylib name is `data_store_demo` (dashes in the
// crate name are replaced with underscores).

val abiToRustTarget = mapOf(
    "arm64-v8a" to "aarch64-linux-android",
    "armeabi-v7a" to "armv7-linux-androideabi",
    "x86_64" to "x86_64-linux-android",
    "x86" to "i686-linux-android",
)

val workspaceRoot: File = project.rootDir.resolve("../../..").normalize()
val rustJniLibsDir = layout.buildDirectory.dir("rustJniLibs")

val workspaceSources: FileTree = fileTree(workspaceRoot) {
    include(
        "crates/**/src/**",
        "crates/**/Cargo.toml",
        "crates/**/build.rs",
        "plugins/**/src/**",
        "plugins/**/Cargo.toml",
        "plugins/**/build.rs",
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
        "examples/*/ios/**",
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

istmoCargoLib("data-store-demo")

// ---- Kotlin runtime deps --------------------------------------------
//
// data-store plugin declares zero external native deps (`SharedPreferences`
// ships with the platform). `dev.istmo:istmo-runtime` is
// substituted for the local `runtime/android` composite build in
// `settings.gradle.kts`; a published `runtime-vX.Y.Z` release resolves the
// same coordinate straight from GitHub Packages.

dependencies {
    implementation("dev.istmo:istmo-runtime:0.1.0")
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
}
