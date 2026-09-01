plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.mozilla.rust-android-gradle.rust-android")
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
        // No `ndk { abiFilters }` — the rust-android-gradle plugin drives
        // ABI selection via its own `targets` list below.
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

// rust-android-gradle plugin: compiles the cdylib crate at
// examples/android-demo/ into per-ABI `.so` files under
// app/build/rustJniLibs/<abi>/. AGP picks them up via the standard JNI
// merge, so no manual `cp` is needed.
cargo {
    module = "../.."                    // from app/ -> examples/android-demo/
    libname = "istmo_android_demo"
    targets = listOf("arm64")
    targetDirectory = "../../target"    // share istmo/target/ with regular cargo
    profile = if (gradle.startParameter.taskNames.any { it.contains("release", ignoreCase = true) }) {
        "release"
    } else {
        "debug"
    }
}

tasks.whenTaskAdded {
    if (name == "mergeDebugJniLibFolders" || name == "mergeReleaseJniLibFolders") {
        dependsOn("cargoBuild")
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
}
