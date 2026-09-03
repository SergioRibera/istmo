import org.gradle.api.tasks.Exec

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.istmo.rustdemo"
    compileSdk = 34

    defaultConfig {
        applicationId = "dev.istmo.rustdemo"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1"
        ndk {
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

android.sourceSets["main"].jniLibs.setSrcDirs(
    listOf(layout.buildDirectory.dir("rustJniLibs").get().asFile),
)

// ---- istmo cargo integration ----------------------------------------
//
// Same recipe the other android demos use. See android-demo for the full
// rationale (Gradle drives cargo, multi-cdylib support, ABI list is the
// single point of change).
//
// The demo's cdylib name is `rust_mobile_demo`. cargo emits
// `librust_mobile_demo.so` because rust-mobile-demo has dashes replaced
// with underscores.

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

istmoCargoLib("rust-mobile-demo")

// ---- Native deps aggregated from the plugins the app consumes -------
//
// Mirrors what `NativeDeps::render_gradle()` would emit for
// [PermissionsClient, NotificationsClient, SignInClient]. Once
// `istmo-build` grows a build-integration recipe, this block gets
// generated from the `istmo::runtime!` declaration.

dependencies {
    // AndroidX + Kotlin runtime.
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")

    // istmo.google_sign_in — Credential Manager + Play Services Auth +
    // Google Identity Services.
    implementation("androidx.credentials:credentials:1.3.0")
    implementation("androidx.credentials:credentials-play-services-auth:1.3.0")
    implementation("com.google.android.libraries.identity.googleid:googleid:1.1.1")
}
