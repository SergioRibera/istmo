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
# registry so repeat builds are fast. Host's ~/.android is bind-mounted
# into /root/.android so AGP's default debug signing config uses the
# SAME debug.keystore the developer sees on the host — critical for
# Google Sign-In / Credential Manager, where the SHA-1 registered in
# Google Cloud Console must match the SHA-1 that signed the APK.
mount := "-v $(pwd):/src -v $HOME/.android:/root/.android -v gradle-cache:/root/.gradle -v cargo-cache:/root/.cargo"

# ---- Demo entry points ----------------------------------------------
#
# `just <demo-name>` is the flutter-run-style workflow: build APK,
# install to device, launch main activity. The parameterised verbs
# below (`build`, `install`, `run`, `clean`) handle the individual
# steps by name.

android-demo: (build "android-demo") (install "android-demo") (run "android-demo")
android-multi: (build "android-multi") (install "android-multi") (run "android-multi")
rust-mobile-demo: (build "rust-mobile-demo") (install "rust-mobile-demo") (run "rust-mobile-demo")

# ---- iOS demo (macOS-only) ------------------------------------------
#
# Uses Xcode + xcodegen + the standard Apple toolchain — no Docker,
# because iOS toolchains do not run in Linux containers. Every recipe
# below fails fast outside macOS.
#
# Prerequisites:
#   * Xcode 15+ with an iOS 14+ simulator installed
#   * `xcodegen` (brew install xcodegen)
#   * `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`
#
# Simulator id is bound at recipe time via `xcrun simctl list`; override
# by passing SIMULATOR_ID as an env var.

ios_project_dir := "examples/ios-demo/ios"
ios_scheme := "IstmoDemo"
ios_bundle := "dev.istmo.demo.IstmoDemo"

# One-shot: regenerate the .xcodeproj from project.yml. Run after any
# change to project.yml, IstmoRuntime/Package.swift, or when a fresh
# checkout does not yet have IstmoDemo.xcodeproj/.
ios-bootstrap:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" != "Darwin" ]]; then
        echo "ios-bootstrap: macOS required"; exit 1
    fi
    cd {{ios_project_dir}} && xcodegen generate

# Build the app for the arm64 simulator. Cargo runs from the Xcode
# pre-build phase; a warm cache turns this into a link-only step.
ios-build:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" != "Darwin" ]]; then
        echo "ios-build: macOS required"; exit 1
    fi
    cd {{ios_project_dir}}
    xcodebuild -project IstmoDemo.xcodeproj \
        -scheme {{ios_scheme}} \
        -configuration Debug \
        -sdk iphonesimulator \
        -destination 'platform=iOS Simulator,name=iPhone 15' \
        build

# Install the built .app into the booted simulator + launch.
ios-run: ios-build
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" != "Darwin" ]]; then
        echo "ios-run: macOS required"; exit 1
    fi
    sim_id="${SIMULATOR_ID:-$(xcrun simctl list devices booted -j | \
        python3 -c 'import json,sys; d=json.load(sys.stdin)["devices"]; \
        print(next(x["udid"] for v in d.values() for x in v if x["state"]=="Booted"))')}"
    app_path=$(find ~/Library/Developer/Xcode/DerivedData -type d -name '{{ios_scheme}}.app' -path '*Debug-iphonesimulator*' | head -n 1)
    if [[ -z "${app_path}" ]]; then
        echo "ios-run: could not locate built .app; run ios-build first"; exit 1
    fi
    xcrun simctl install "${sim_id}" "${app_path}"
    xcrun simctl launch "${sim_id}" {{ios_bundle}}

# Wipe cargo + Xcode build output for the iOS demo. Doesn't touch the
# workspace-wide target/ dir; only the demo's staging area under
# examples/ios-demo/ios/build/.
ios-clean:
    rm -rf {{ios_project_dir}}/build
    rm -rf {{ios_project_dir}}/IstmoDemo.xcodeproj

# Full "flutter run"-style: bootstrap (if needed) + build + install + launch.
ios-demo:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -d "{{ios_project_dir}}/IstmoDemo.xcodeproj" ]]; then
        just ios-bootstrap
    fi
    just ios-run

# ---- rust-mobile-demo iOS (macOS-only) ------------------------------
#
# Parallels the ios-demo recipes above but points at the full-Rust demo's
# iOS build system. Same tooling requirements (Xcode 15+, xcodegen,
# `rustup target add aarch64-apple-ios{,-sim}`).

rmd_ios_project_dir := "examples/rust-mobile-demo/ios"
rmd_ios_scheme := "RustMobileDemo"
rmd_ios_bundle := "dev.istmo.rustmobile.RustMobileDemo"

rust-mobile-ios-bootstrap:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" != "Darwin" ]]; then
        echo "rust-mobile-ios-bootstrap: macOS required"; exit 1
    fi
    cd {{rmd_ios_project_dir}} && xcodegen generate

rust-mobile-ios-build:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" != "Darwin" ]]; then
        echo "rust-mobile-ios-build: macOS required"; exit 1
    fi
    cd {{rmd_ios_project_dir}}
    xcodebuild -project RustMobileDemo.xcodeproj \
        -scheme {{rmd_ios_scheme}} \
        -configuration Debug \
        -sdk iphonesimulator \
        -destination 'platform=iOS Simulator,name=iPhone 15' \
        build

rust-mobile-ios-run: rust-mobile-ios-build
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ "$(uname -s)" != "Darwin" ]]; then
        echo "rust-mobile-ios-run: macOS required"; exit 1
    fi
    sim_id="${SIMULATOR_ID:-$(xcrun simctl list devices booted -j | \
        python3 -c 'import json,sys; d=json.load(sys.stdin)["devices"]; \
        print(next(x["udid"] for v in d.values() for x in v if x["state"]=="Booted"))')}"
    app_path=$(find ~/Library/Developer/Xcode/DerivedData -type d -name '{{rmd_ios_scheme}}.app' -path '*Debug-iphonesimulator*' | head -n 1)
    if [[ -z "${app_path}" ]]; then
        echo "rust-mobile-ios-run: could not locate built .app; run rust-mobile-ios-build first"; exit 1
    fi
    xcrun simctl install "${sim_id}" "${app_path}"
    xcrun simctl launch "${sim_id}" {{rmd_ios_bundle}}

rust-mobile-ios-clean:
    rm -rf {{rmd_ios_project_dir}}/build
    rm -rf {{rmd_ios_project_dir}}/RustMobileDemo.xcodeproj

rust-mobile-ios:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ ! -d "{{rmd_ios_project_dir}}/RustMobileDemo.xcodeproj" ]]; then
        just rust-mobile-ios-bootstrap
    fi
    just rust-mobile-ios-run

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
        rust-mobile-demo) component="dev.istmo.rustdemo/dev.istmo.rustdemo.RustMobileActivity" ;;
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

# Gen default keystore default
# keytool -genkey -v -keystore debug.keystore -storepass android -alias androiddebugkey -keypass android -keyalg RSA -keysize 2048 -validity 10000 -dname "C=US, O=Android, CN=Android Debug"
helper:
    docker run --rm {{mount}} \
        -w /src \
        --entrypoint sh {{image}} \
        -c 'keytool -list -v -keystore /root/.android/debug.keystore -alias androiddebugkey -storepass android -keypass android'
