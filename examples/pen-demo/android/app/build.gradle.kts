plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    // Builds the Rust crate for every ABI and build type, links the istmo
    // plugins it depends on and applies istmo.toml `[app]` (id, version,
    // minSdk, label, icon).
    id("dev.istmo.app")
}

android {
    namespace = "dev.istmo.pendemo"
    compileSdk = 34

    defaultConfig {
        targetSdk = 34
        ndk {
            abiFilters += setOf("arm64-v8a")
        }
    }

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

dependencies {
    implementation("dev.istmo:istmo-runtime:0.1.0")
    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
    // GameActivity dispatches MotionEvents through Java first, so a
    // Kotlin `dispatchTouchEvent` override can spy on stylus samples
    // before the events reach the native (winit/eframe) side. Required
    // by the `android-game-activity` feature enabled on winit + eframe
    // in `Cargo.toml`.
    implementation("androidx.games:games-activity:4.4.0")
}
