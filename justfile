# Task runner for the istmo framework.
#
# Docker image: sergioribera/rust-android:1.96-sdk-37.0.
# * Ships Rust 1.96, Android SDK/NDK and Gradle 8.2.
# * NDK toolchain env vars are pre-set for every android target, so plain
#   `cargo build --target <triple>` links. No cargo-ndk.
#
# Design: Gradle is the build root. `gradle assembleDebug` triggers the
# `cargoBuild_*` + `stageRustLib_*` tasks (declared via `istmoCargoLib`
# in the demo's app/build.gradle.kts), which cross-compile the workspace
# cdylibs and stage the .so files under $buildDir/rustJniLibs/<abi>/.
# Adding an ABI = touch `abiFilters` in the demo's app/build.gradle.kts.

image := "sergioribera/rust-android:1.96-sdk-37.0"

# Mount repo root at /src. Named volumes cache Gradle downloads + cargo
# registry so repeat builds are fast.
mount := "-v $(pwd):/src -v gradle-cache:/root/.gradle -v cargo-cache:/root/.cargo"

# ---- Demo entry points ----------------------------------------------
#
# `just <demo-name>` is the flutter-run-style workflow: build APK,
# install to device, launch main activity. The parameterised verbs
# below (`build`, `install`, `run`, `clean`) handle the individual
# steps by name.

android-demo: (build "android-demo") (install "android-demo") (run "android-demo")
android-multi: (build "android-multi") (install "android-multi") (run "android-multi")

# ---- Parameterised recipes ------------------------------------------

# Build the demo's APK inside the container. Gradle drives cargo per
# the recipe in the demo's app/build.gradle.kts.
build name:
    docker run --rm -it {{mount}} \
        -w /src/examples/{{name}}/android \
        --entrypoint bash {{image}} \
        -c 'gradle :app:assembleDebug --no-daemon'

# Install the last-built debug APK on a connected device / emulator.
install name:
    adb install -r examples/{{name}}/android/app/build/outputs/apk/debug/app-debug.apk

# Launch the demo's main activity. (package, activity) mapping is kept
# here so `just run <demo>` works without parsing manifests. Add a new
# arm per demo.
run name:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{name}}" in
        android-demo) component="dev.istmo.demo/dev.istmo.demo.MainActivity" ;;
        android-multi) component="dev.istmo.multi/dev.istmo.multi.MainActivity" ;;
        *) echo "just run: no launcher configured for '{{name}}'"; exit 1 ;;
    esac
    adb shell am start -n "$component"

# Wipe Gradle output for the demo.
clean name:
    docker run --rm -it {{mount}} \
        -w /src/examples/{{name}}/android \
        --entrypoint bash {{image}} \
        -c 'gradle clean --no-daemon'

# One-shot: delete root-owned files left by previous docker builds
# (e.g. legacy app/src/main/jniLibs/ from an earlier justfile revision).
# Runs `rm -rf` inside the container so root perms are respected.
reset-owned name paths:
    docker run --rm -v $(pwd):/src \
        -w /src/examples/{{name}}/android \
        --entrypoint sh {{image}} \
        -c 'rm -rf {{paths}}'
