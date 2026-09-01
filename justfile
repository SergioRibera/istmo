# Task runner for the istmo framework.
#
# Docker image: sergioribera/rust-android:1.96-sdk-37.0.
# * Ships Rust 1.96, Android SDK/NDK and Gradle 8.2.
# * Cross-compiles to `aarch64-linux-android` out of the box — the NDK
#   toolchain env vars are set in the image profile, so plain `cargo
#   build --target aarch64-linux-android` links correctly. No cargo-ndk.

image := "sergioribera/rust-android:1.96-sdk-37.0"

# ABI shipped by the demo. Extend `android/app/build.gradle.kts`'s
# `abiFilters` and add matching cargo targets before adding more.
abi := "arm64-v8a"
rust_target := "aarch64-linux-android"

# Mount the repo root at /src. Named volumes cache the Gradle download
# tree and the cargo registry so repeat builds are fast.
mount := "-v $(pwd):/src -v gradle-cache:/root/.gradle -v cargo-cache:/root/.cargo"

# ---- Android demo ----------------------------------------------------

# Full demo build: Rust cdylib -> jniLibs/<abi>/ -> debug APK.
android-demo: android-demo-so android-demo-apk

# Cross-compile the cdylib and drop it under jniLibs so the APK picks
# it up. Both steps run in the same container invocation to avoid two
# cold container starts.
android-demo-so:
    docker run --rm -it {{mount}} \
        -w /src \
        --entrypoint bash {{image}} \
        -c 'cargo build --release --target {{rust_target}} -p istmo-android-demo && \
            mkdir -p examples/android-demo/android/app/src/main/jniLibs/{{abi}} && \
            cp target/{{rust_target}}/release/libistmo_android_demo.so \
               examples/android-demo/android/app/src/main/jniLibs/{{abi}}/'

# System Gradle (8.2) matches the AGP 8.2.2 pinned in
# android/build.gradle.kts. --no-daemon: no long-lived JVM in the
# ephemeral container.
android-demo-apk:
    docker run --rm -it {{mount}} \
        -w /src/examples/android-demo/android \
        --entrypoint bash {{image}} \
        -c 'gradle :app:assembleDebug --no-daemon'

android-demo-clean:
    docker run --rm -it {{mount}} \
        -w /src/examples/android-demo/android \
        --entrypoint bash {{image}} \
        -c 'gradle clean --no-daemon'

# adb install the last debug APK. Requires an emulator/device visible to
# the host adb.
android-demo-install:
    adb install -r examples/android-demo/android/app/build/outputs/apk/debug/app-debug.apk

android-demo-run: android-demo android-demo-install
