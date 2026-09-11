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

    // Pin the debug signing config so it always resolves to the
    // developer's `~/.android/debug.keystore` — inside Docker that path
    // is bind-mounted from the host via the `mount` variable in the
    // workspace `justfile`. Without this explicit block AGP would fall
    // back to creating a fresh keystore inside the container on first
    // build, whose SHA-1 wouldn't match the one registered against the
    // Google OAuth Android client id.
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
    // Canonical Kotlin runtime — pulled from `runtime/android` through a
    // Gradle composite build in `settings.gradle.kts`; the same
    // coordinate resolves from GitHub Packages once `runtime-vX.Y.Z` is
    // published.
    implementation("dev.istmo:istmo-runtime-android:0.1.0")

    // AndroidX + Kotlin runtime.
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
    // `Task<T>.await()` extension used by GoogleSignInHandler.
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-play-services:1.8.1")

    // istmo.google_sign_in — Legacy Google Sign-In (play-services-auth).
    //
    // Chose the legacy client over Credential Manager because on MIUI
    // (and other aggressive process managers) the Play-Services-launched
    // Activity that both `GetSignInWithGoogleOption` and
    // `GetGoogleIdOption` rely on gets killed mid-flow, surfacing as a
    // bogus `TYPE_USER_CANCELED` right after the user selects an
    // account. `GoogleSignInClient.signInIntent` starts the picker from
    // the calling Activity directly (plain `startActivityForResult`),
    // sidestepping the killer.
    implementation("com.google.android.gms:play-services-auth:21.3.0")

    // istmo.admob — Google Mobile Ads SDK. Provides InterstitialAd,
    // RewardedAd, AdView + the MobileAds initialiser.
    implementation("com.google.android.gms:play-services-ads:23.6.0")
}
