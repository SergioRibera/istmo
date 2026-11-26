plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    // Builds the Rust crate, links the istmo plugins it depends on
    // (screen-capture's Kotlin backend, service and permissions) and
    // applies `istmo.toml` `[app]`.
    id("io.github.sergioribera.istmo")
}

android {
    namespace = "dev.istmo.screencaptureprobe"
    compileSdk = 34

    defaultConfig {
        targetSdk = 34
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

dependencies {
    implementation("io.github.sergioribera:istmo-runtime:0.1.1-alpha.7")
    implementation("androidx.activity:activity:1.8.1")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
}
