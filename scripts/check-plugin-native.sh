#!/bin/sh
# Compile a plugin's `native/` backends against the Kotlin / Swift its
# contract generates (the plugin's own `tests/golden/*.expected`; Kotlin
# from the whole app files, `app_<prefix>_*.kt.expected`), with
# warnings as errors, on a plain JVM / any Swift toolchain: the runtime
# sources plus the stubs under `runtime/*/ci` stand in for the Android
# and Apple SDKs. Catches a backend that drifted from its contract
# without a device or an SDK. The Swift side also carries every other
# plugin's generated Swift, as one app enabling them all would.
#
# usage: scripts/check-plugin-native.sh <plugin dir name> <kotlin package> <golden prefix>
#   e.g. scripts/check-plugin-native.sh screen-capture dev.istmo.plugins.screencapture screen_capture
#   KOTLINC / SWIFTC   compiler commands (default `kotlinc` / `swiftc`)
#   KOTLIN_CLASSPATH   at least kotlinx-coroutines-core-jvm
#   SKIP_KOTLIN / SKIP_SWIFT  set to skip one side
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
plugin=$root/plugins/$1
package=$2
prefix=$3
golden=$plugin/tests/golden
shared=$root/crates/media/tests/golden
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ -z "${SKIP_KOTLIN:-}" ]; then
    runtime="$root/runtime/android/src/main/java/dev/istmo/runtime"
    ci="$root/runtime/android/ci"
    mkdir "$work/kt"
    # The files as an app receives them (`tests/golden/app_*`): package
    # line and the imports `istmo-build` writes, each shared type by name.
    for part in types codecs host; do
        app="$golden/app_${prefix}_$part.kt.expected"
        if [ "$(head -n 1 "$app")" != "package $package" ]; then
            echo "$app is not in package $package" >&2
            exit 1
        fi
        cp "$app" "$work/kt/${prefix}_$part.kt"
    done
    # The shared media types, generated once per app in their package.
    { echo "package dev.istmo.media"; echo; cat "$shared/istmo_media_types.kt.expected"; } \
        > "$work/kt/istmo_media_types.kt"
    echo "compiling $1 native/android against its generated Kotlin"
    # shellcheck disable=SC2086 # KOTLINC may carry its own arguments.
    ${KOTLINC:-kotlinc} -Werror ${KOTLIN_CLASSPATH:+-classpath "$KOTLIN_CLASSPATH"} -d "$work/kt-out" \
        "$runtime/Bincode.kt" "$runtime/PluginHandler.kt" "$runtime/IstmoFrameSender.kt" "$ci"/*.kt \
        "$work/kt"/*.kt "$plugin"/native/android/*.kt
fi

if [ -z "${SKIP_SWIFT:-}" ]; then
    swiftc=${SWIFTC:-swiftc}
    ci="$root/runtime/ios/ci"
    src="$root/runtime/ios/Sources/IstmoRuntime"
    ffi="$root/runtime/ios/Sources/IstmoFFI/include"
    module() {
        name=$1
        shift
        "$swiftc" -parse-as-library -emit-module -module-name "$name" \
            -I "$work" -I "$ffi" -emit-module-path "$work/$name.swiftmodule" "$@"
    }
    module Foundation "$ci/FoundationStub.swift"
    module IstmoRuntime "$src/Bincode.swift" "$src/PluginException.swift" \
        "$src/PluginHandler.swift" "$src/IstmoRuntime.swift" "$src/IstmoFrameSender.swift"
    mkdir "$work/swift"
    # Every plugin with Swift goldens (this one included) shares the app
    # module, as in an app enabling them all: a backend naming another
    # plugin's type unqualified, or a namespace clash, fails here.
    for host in "$root"/plugins/*/tests/golden/*_host.swift.expected; do
        other=${host%_host.swift.expected}
        for part in types codecs host; do
            cp "${other}_$part.swift.expected" "$work/swift/$(basename "$other")_$part.swift"
        done
    done
    cp "$shared/istmo_media_types.swift.expected" "$work/swift/istmo_media_types.swift"
    echo "type-checking $1 native/ios against the generated Swift of every plugin"
    "$swiftc" -typecheck -warnings-as-errors -I "$work" -I "$ffi" -module-name App \
        "$work/swift"/*.swift "$plugin"/native/ios/*.swift
fi
echo "$1 native backends compile against the generated code"
