# Task runner for the istmo framework.
#
# Docker image: sergioribera/rust-android:1.96-sdk-37.0.
# * Ships Rust 1.96, Android SDK/NDK and Gradle 8.2.
# * Cross-compilation is handled by the mozilla `rust-android-gradle`
#   plugin inside each demo's `android/app/build.gradle.kts` — the ABI
#   list, Rust target and `.so` placement live there, not here.

image := "sergioribera/rust-android:1.96-sdk-37.0"

# Mount repo root at /src. Named volumes cache the Gradle download tree
# and the cargo registry so repeat builds are fast.
mount := "-v $(pwd):/src -v gradle-cache:/root/.gradle -v cargo-cache:/root/.cargo"

# ---- Demo entry points ----------------------------------------------
#
# One alias per demo: `just <demo-name>` is the full flutter-run-style
# workflow (build APK, install to device, launch main activity).
#
# The parameterised verbs below (`build`, `install`, `run`, `clean`)
# take a demo name so partial workflows are still one-liners.

# `just android-demo` — build + install + launch the M2 echo demo.
android-demo: (build "android-demo") (install "android-demo") (run "android-demo")

# ---- Parameterised recipes ------------------------------------------

# Build the demo's APK inside the container. `gradle assembleDebug`
# triggers `cargoBuild` first (wired in the demo's app/build.gradle.kts).
build name:
    docker run --rm -it {{mount}} \
        -w /src/examples/{{name}}/android \
        --entrypoint bash {{image}} \
        -c 'gradle :app:assembleDebug --no-daemon'

# Install the last-built debug APK on a connected device / emulator.
install name:
    adb install -r examples/{{name}}/android/app/build/outputs/apk/debug/app-debug.apk

# Launch the demo's main activity. The (package, activity) mapping is
# maintained here so `just run <demo>` works without parsing manifests.
# Add a new arm when a new demo lands.
run name:
    #!/usr/bin/env bash
    set -euo pipefail
    case "{{name}}" in
        android-demo) component="dev.istmo.demo/dev.istmo.demo.MainActivity" ;;
        *) echo "just run: no launcher configured for '{{name}}'"; exit 1 ;;
    esac
    adb shell am start -n "$component"

# Wipe Gradle output for the demo.
clean name:
    docker run --rm -it {{mount}} \
        -w /src/examples/{{name}}/android \
        --entrypoint bash {{image}} \
        -c 'gradle clean --no-daemon'
