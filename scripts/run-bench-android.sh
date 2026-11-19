#!/usr/bin/env bash
# Build the bench APK, install it on an attached Android device, run the
# matrix and capture the output into `results/<device>-<timestamp>.txt`.
#
# Requires `adb`, `cargo ndk`, `gradle`, the Android SDK/NDK and JDK 17
# on the PATH. The easiest way to get all of them at once is to invoke
# this script from inside `nix develop .#bench-android`.

set -euo pipefail

usage() {
    cat <<EOF
Usage: $(basename "$0") [--serial <id>] [--abi <arm64-v8a|x86_64|...>] [--skip-build]

  --serial <id>   ADB device id (needed when more than one is attached).
  --abi <abi>     Build + install only the given ABI (default: arm64-v8a).
  --skip-build    Reuse an already-installed APK, just launch + capture.
EOF
    exit "${1:-0}"
}

serial=""
abi="arm64-v8a"
skip_build=0

while [ "$#" -gt 0 ]; do
    case "$1" in
        --serial) serial="$2"; shift 2 ;;
        --abi) abi="$2"; shift 2 ;;
        --skip-build) skip_build=1; shift ;;
        -h|--help) usage 0 ;;
        *) echo "unknown arg: $1" >&2; usage 1 ;;
    esac
done

# Resolve the repo root. When the script is installed into the Nix
# store (via `nix run .#bench-android-device`), `$0` points at the
# store copy and its parent is read-only; honour `$ISTMO_REPO` first,
# then fall back to the working directory.
if [ -n "${ISTMO_REPO:-}" ]; then
    repo_root="$ISTMO_REPO"
elif [ -f "$PWD/Cargo.toml" ] && grep -q '^\[workspace\]' "$PWD/Cargo.toml" 2>/dev/null; then
    repo_root="$PWD"
else
    echo "cannot find the istmo workspace: run this from the repo root or set ISTMO_REPO" >&2
    exit 2
fi
bench_dir="$repo_root/examples/bench-android"
results_dir="$repo_root/results/bench-android"
mkdir -p "$results_dir"

command -v adb >/dev/null || { echo "adb not on PATH (enter nix develop .#bench-android?)" >&2; exit 2; }

if [ -n "$serial" ]; then
    adb_flags=(-s "$serial")
else
    devices=$(adb devices | awk 'NR>1 && $2=="device" {print $1}')
    count=$(echo "$devices" | grep -c . || true)
    if [ "$count" -eq 0 ]; then
        echo "no adb devices attached (adb devices)" >&2
        exit 2
    elif [ "$count" -gt 1 ]; then
        echo "$count devices attached; pick one with --serial:" >&2
        echo "$devices" >&2
        exit 2
    fi
    serial="$devices"
    adb_flags=(-s "$serial")
fi

echo "::  using device:   $serial"
model=$(adb "${adb_flags[@]}" shell getprop ro.product.model | tr -d '\r\n ' | tr '[:upper:]' '[:lower:]')
release=$(adb "${adb_flags[@]}" shell getprop ro.build.version.release | tr -d '\r\n')
echo "::  model:          $model"
echo "::  android:        $release"

if [ "$skip_build" -eq 0 ]; then
    echo "::  building rust cdylib for $abi"
    command -v cargo >/dev/null || { echo "cargo not on PATH" >&2; exit 2; }
    command -v cargo-ndk >/dev/null || { echo "cargo-ndk not on PATH" >&2; exit 2; }
    (
        cd "$bench_dir"
        cargo ndk -t "$abi" -o android/app/src/main/jniLibs build \
            -p bench-android --release
    )
    echo "::  assembling apk"
    # Prefer the project's Gradle wrapper when it exists; otherwise
    # fall back to the system `gradle` from the devshell. First-time
    # setup drops a wrapper beside the project so repeat runs are
    # version-pinned.
    (
        cd "$bench_dir/android"
        if [ -x ./gradlew ]; then
            ./gradlew :app:assembleRelease
        else
            gradle wrapper --gradle-version 8.10.2 >/dev/null
            ./gradlew :app:assembleRelease
        fi
    )
fi

apk="$bench_dir/android/app/build/outputs/apk/release/app-release-unsigned.apk"
[ -f "$apk" ] || { echo "apk not found: $apk" >&2; exit 2; }

echo "::  installing"
adb "${adb_flags[@]}" install -r "$apk" >/dev/null

echo "::  clearing logcat"
adb "${adb_flags[@]}" logcat -c

echo "::  launching activity (auto mode)"
adb "${adb_flags[@]}" shell am start -W -n dev.istmo.bench/.BenchActivity \
    --ez auto true >/dev/null

timestamp=$(date -u +%Y%m%dT%H%M%SZ)
out="$results_dir/$model-$timestamp.txt"

echo "::  capturing logcat → $out"
# Filter to our tag at level I, silence everything else. Stop at
# BENCH_END so the pipe closes cleanly instead of hanging.
adb "${adb_flags[@]}" logcat -v raw istmo-bench:I '*:S' \
    | while IFS= read -r line; do
        echo "$line" >> "$out"
        case "$line" in
            *BENCH_END*) echo "$line"; break ;;
            *) echo "$line" ;;
        esac
    done

echo
echo "::  results captured to $out"
