#!/usr/bin/env bash
# Build the Flutter bench APK, install it, auto-run on the attached
# device and capture the result table from logcat.
#
# Requires `flutter`, `adb`, the Android SDK/NDK, and JDK 17 on the
# PATH. Entering `nix develop .#bench-flutter` gives all of them at
# once. The example project's `android/` and `ios/` scaffolding has to
# exist first — run `flutter create --org dev.istmo --project-name
# bench_flutter .` once and merge our AndroidManifest / MainActivity /
# AppDelegate on top of the generated tree.

set -euo pipefail

usage() {
    cat <<EOF
Usage: $(basename "$0") [--serial <id>] [--skip-build]

  --serial <id>   ADB device id (needed when more than one is attached).
  --skip-build    Reuse an already-installed APK, just launch + capture.
EOF
    exit "${1:-0}"
}

serial=""
skip_build=0

while [ "$#" -gt 0 ]; do
    case "$1" in
        --serial) serial="$2"; shift 2 ;;
        --skip-build) skip_build=1; shift ;;
        -h|--help) usage 0 ;;
        *) echo "unknown arg: $1" >&2; usage 1 ;;
    esac
done

if [ -n "${ISTMO_REPO:-}" ]; then
    repo_root="$ISTMO_REPO"
elif [ -f "$PWD/Cargo.toml" ] && grep -q '^\[workspace\]' "$PWD/Cargo.toml" 2>/dev/null; then
    repo_root="$PWD"
else
    echo "cannot find the istmo workspace: run this from the repo root or set ISTMO_REPO" >&2
    exit 2
fi
bench_dir="$repo_root/examples/bench-flutter"
results_dir="$repo_root/results/bench-flutter"
mkdir -p "$results_dir"

command -v adb >/dev/null || { echo "adb not on PATH" >&2; exit 2; }
command -v flutter >/dev/null || { echo "flutter not on PATH" >&2; exit 2; }

if [ ! -d "$bench_dir/android" ]; then
    echo "bench-flutter android/ scaffold missing; run:" >&2
    echo "  cd $bench_dir && flutter create --org dev.istmo --project-name bench_flutter ." >&2
    echo "then merge our AndroidManifest + MainActivity on top of the generated tree." >&2
    exit 2
fi

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
    echo "::  flutter pub get"
    (cd "$bench_dir" && flutter pub get >/dev/null)
    echo "::  flutter build apk --release"
    (cd "$bench_dir" && flutter build apk --release)
fi

apk="$bench_dir/build/app/outputs/flutter-apk/app-release.apk"
[ -f "$apk" ] || { echo "apk not found: $apk" >&2; exit 2; }

echo "::  installing"
adb "${adb_flags[@]}" install -r "$apk" >/dev/null

echo "::  clearing logcat"
adb "${adb_flags[@]}" logcat -c

echo "::  launching activity (auto mode)"
adb "${adb_flags[@]}" shell am start -W \
    -n dev.istmo.benchflutter/.MainActivity \
    --ez auto true >/dev/null

timestamp=$(date -u +%Y%m%dT%H%M%SZ)
out="$results_dir/$model-$timestamp.txt"

echo "::  capturing logcat → $out"
adb "${adb_flags[@]}" logcat -v raw istmo-flutter-bench:I '*:S' \
    | while IFS= read -r line; do
        echo "$line" >> "$out"
        case "$line" in
            *BENCH_END*) echo "$line"; break ;;
            *) echo "$line" ;;
        esac
    done

echo
echo "::  results captured to $out"
