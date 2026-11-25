#!/usr/bin/env bash
# Build the bench APK, install it on an attached Android device, run the
# latency matrix and capture it into
# `results/mobile-latency/<device>-<timestamp>.txt`.
#
# Each mode runs in its own process (the istmo runtime starts once per
# process): `full` (the matrix, runtime started the way apps start it),
# `probe` (per-hop breakdown, pump callbacks stamped by the bench) and
# `legacy` (the original batch-mean matrix) and `spin` (spin-before-park
# on and off, idle and with every core busy).
#
# Requires `adb`, `cargo ndk`, `gradle`, the Android SDK/NDK and JDK 17
# on the PATH: `nix run .#bench-android-device` provides all of them.

set -euo pipefail

usage() {
    cat <<USAGE
Usage: $(basename "$0") [--serial <id>] [--abi <abi>] [--skip-build] [--modes "full probe legacy"]

  --serial <id>   ADB device id (needed when more than one is attached).
  --abi <abi>     Build + install only the given ABI (default: arm64-v8a).
  --skip-build    Reuse the APK already built, just install + run.
  --modes <list>  Which runs to do, in order (default: "full probe legacy spin").
USAGE
    exit "${1:-0}"
}

serial=""
abi="arm64-v8a"
skip_build=0
modes="full probe legacy spin"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --serial) serial="$2"; shift 2 ;;
        --abi) abi="$2"; shift 2 ;;
        --skip-build) skip_build=1; shift ;;
        --modes) modes="$2"; shift 2 ;;
        -h|--help) usage 0 ;;
        *) echo "unknown arg: $1" >&2; usage 1 ;;
    esac
done

# `$0` lives in the Nix store under `nix run`: resolve the checkout from
# `$ISTMO_REPO` or the working directory.
if [ -n "${ISTMO_REPO:-}" ]; then
    repo_root="$ISTMO_REPO"
elif [ -f "$PWD/Cargo.toml" ] && grep -q '^\[workspace\]' "$PWD/Cargo.toml" 2>/dev/null; then
    repo_root="$PWD"
else
    echo "cannot find the istmo workspace: run this from the repo root or set ISTMO_REPO" >&2
    exit 2
fi
bench_dir="$repo_root/examples/bench-android"
results_dir="$repo_root/results/mobile-latency"
mkdir -p "$results_dir"

command -v adb >/dev/null || { echo "adb not on PATH (enter nix develop .#bench-android?)" >&2; exit 2; }

if [ -z "$serial" ]; then
    devices=$(adb devices | awk 'NR>1 && $2=="device" {print $1}')
    count=$(echo "$devices" | grep -c . || true)
    if [ "$count" -ne 1 ]; then
        echo "$count devices attached; pick one with --serial:" >&2
        echo "$devices" >&2
        exit 2
    fi
    serial="$devices"
fi
adb_cmd() { adb -s "$serial" "$@"; }

model=$(adb_cmd shell getprop ro.product.model | tr -d '\r\n ' | tr '[:upper:]' '[:lower:]')
echo "::  device:         $serial ($model, android $(adb_cmd shell getprop ro.build.version.release | tr -d '\r\n'))"

if [ "$skip_build" -eq 0 ]; then
    echo "::  building rust cdylib for $abi"
    (
        cd "$bench_dir"
        cargo ndk -t "$abi" -o android/app/src/main/jniLibs build -p bench-android --release
    )
    echo "::  assembling apk"
    # Nixpkgs' Gradle hard-codes a patched `aapt2` path that does not
    # survive a store GC; point it at the active build-tools' one.
    gradle_aapt2_flag=()
    if [ -n "${ANDROID_SDK_ROOT:-}" ]; then
        aapt2=$(ls "$ANDROID_SDK_ROOT"/build-tools/*/aapt2 2>/dev/null | sort -V | tail -n1 || true)
        [ -n "$aapt2" ] && gradle_aapt2_flag=(-Pandroid.aapt2FromMavenOverride="$aapt2")
    fi
    (cd "$bench_dir/android" && ./gradlew "${gradle_aapt2_flag[@]}" :app:assembleRelease)
fi

apk="$bench_dir/android/app/build/outputs/apk/release/app-release.apk"
[ -f "$apk" ] || apk="$bench_dir/android/app/build/outputs/apk/release/app-release-unsigned.apk"
[ -f "$apk" ] || { echo "apk not found under release/" >&2; exit 2; }
echo "::  installing"
adb_cmd install -r "$apk" >/dev/null

# Keep the device awake while plugged in, and put the setting back on
# exit. Some vendor builds (MIUI) refuse this from adb; the activity
# also holds FLAG_KEEP_SCREEN_ON, so the run proceeds either way.
stay_on_before=$(adb_cmd shell settings get global stay_on_while_plugged_in | tr -d '\r\n')
screen_off_before=$(adb_cmd shell settings get system screen_off_timeout | tr -d '\r\n')
# Long enough for every mode; put back on exit.
adb_cmd shell settings put system screen_off_timeout 1800000 >/dev/null 2>&1 || true
adb_cmd shell svc power stayon usb >/dev/null 2>&1 || echo "::  note: svc power stayon refused; relying on FLAG_KEEP_SCREEN_ON"
sampler_pid=""
restore() {
    [ -n "$sampler_pid" ] && kill "$sampler_pid" 2>/dev/null || true
    case "$screen_off_before" in
        ""|null) ;;
        *) adb_cmd shell settings put system screen_off_timeout "$screen_off_before" >/dev/null 2>&1 || true ;;
    esac
    case "$stay_on_before" in
        0) adb_cmd shell svc power stayon false >/dev/null 2>&1 || true ;;
        *) adb_cmd shell settings put global stay_on_while_plugged_in "$stay_on_before" >/dev/null 2>&1 || true ;;
    esac
}
trap restore EXIT

awake=$(adb_cmd shell dumpsys power | grep -o 'mWakefulness=[A-Za-z]*' | head -n1)
keyguard=$(adb_cmd shell dumpsys window | grep -o 'isKeyguardShowing=[a-z]*' | head -n1 || true)
echo "::  power:          $awake $keyguard"
if [ "$awake" != "mWakefulness=Awake" ] || [ "$keyguard" = "isKeyguardShowing=true" ]; then
    echo "device is asleep or locked: wake and unlock it, then re-run" >&2
    exit 3
fi

timestamp=$(date -u +%Y%m%dT%H%M%SZ)
out="$results_dir/$model-$timestamp.txt"
freq_log="$results_dir/$model-$timestamp.freq.txt"
echo "::  results:        $out"

# Frequencies, thermal status and battery temperature every 2 s, from
# the shell (the app sandbox cannot always read cpufreq).
(
    while true; do
        line=$(adb_cmd shell 'f=""; for c in /sys/devices/system/cpu/cpu[0-9]*; do f="$f $(basename $c):$(cat $c/cpufreq/scaling_cur_freq 2>/dev/null)"; done; t=$(dumpsys thermalservice 2>/dev/null | grep -m1 "Thermal Status" | tr -d " "); b=$(dumpsys battery | grep -m1 temperature | tr -d " "); echo "$f $t $b"' 2>/dev/null | tr -d '\r')
        echo "$(date -u +%H:%M:%S) $line" >> "$freq_log"
        sleep 2
    done
) &
sampler_pid=$!

{
    echo "# governors: $(adb_cmd shell 'for c in /sys/devices/system/cpu/cpu[0-9]*; do printf "%s:%s " $(basename $c) $(cat $c/cpufreq/scaling_governor 2>/dev/null); done' | tr -d '\r')"
    echo "# thermal: $(adb_cmd shell dumpsys thermalservice 2>/dev/null | grep -m1 'Thermal Status' | tr -d '\r')"
} >> "$out"

for mode in $modes; do
    echo "::  running mode:   $mode"
    adb_cmd shell am force-stop dev.istmo.bench
    adb_cmd logcat -c
    adb_cmd shell am start -W -n dev.istmo.bench/.BenchActivity --ez auto true --es mode "$mode" >/dev/null
    # Our tag only, into a file polled for BENCH_END (or 30 minutes):
    # piping `logcat` into a loop that stops reading leaves it blocked
    # until its next write, which never comes.
    capture=$(mktemp)
    adb -s "$serial" logcat -v raw istmo-bench:I '*:S' > "$capture" &
    logcat_pid=$!
    deadline=$((SECONDS + 1800))
    shown=0
    until grep -q BENCH_END "$capture" || [ "$SECONDS" -ge "$deadline" ]; do
        sleep 2
        lines=$(wc -l < "$capture")
        [ "$lines" -gt "$shown" ] && sed -n "$((shown + 1)),${lines}p" "$capture" && shown=$lines
    done
    kill "$logcat_pid" 2>/dev/null || true
    wait "$logcat_pid" 2>/dev/null || true
    sed -n "$((shown + 1)),\$p" "$capture"
    grep -q BENCH_END "$capture" || echo "::  mode $mode timed out" | tee -a "$capture"
    cat "$capture" >> "$out"
    rm -f "$capture"
done
adb_cmd shell am force-stop dev.istmo.bench

echo
echo "::  results captured to $out (frequencies: $freq_log)"
