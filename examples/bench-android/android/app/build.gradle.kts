plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.istmo.bench"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.istmo.bench"
        minSdk = 24
        targetSdk = 35
        versionCode = 1
        versionName = "0.1"
        ndk {
            // Add 32-bit ABIs when targeting older devices.
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }
}

dependencies {
    // No istmo-gradle wiring here: the benchmark bypasses the plugin
    // system entirely and talks to a tiny Rust harness through the
    // JNI bridge defined in `BenchNative.kt`. The cdylib must be built
    // separately (see README) and dropped into
    // `app/src/main/jniLibs/<abi>/libbench_android.so`.
}
