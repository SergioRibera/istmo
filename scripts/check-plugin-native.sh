#!/bin/sh
# Compile a plugin's `native/` backends against the Kotlin / Swift its
# contract generates (the plugin's own `tests/golden/*.expected`), with
# warnings as errors, on a plain JVM / any Swift toolchain: the runtime
# sources plus the stubs under `runtime/*/ci` stand in for the Android
# and Apple SDKs. Catches a backend that drifted from its contract
# without a device or an SDK.
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
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ -z "${SKIP_KOTLIN:-}" ]; then
    runtime="$root/runtime/android/src/main/java/dev/istmo/runtime"
    ci="$root/runtime/android/ci"
    mkdir "$work/kt"
    for part in types codecs host; do
        {
            echo "package $package"
            echo
            # What `istmo-build` imports into a plugin package.
            for name in BackendException Bincode HandleReleaser IstmoBytesOut IstmoFrameSender \
                IstmoGeneratedApi NativeHandleId PluginException PluginHandler PluginResult; do
                echo "import dev.istmo.runtime.$name"
            done
            echo
            cat "$golden/${prefix}_$part.kt.expected"
        } > "$work/kt/${prefix}_$part.kt"
    done
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
    for part in types codecs host; do
        cp "$golden/${prefix}_$part.swift.expected" "$work/swift/${prefix}_$part.swift"
    done
    echo "type-checking $1 native/ios against its generated Swift"
    "$swiftc" -typecheck -warnings-as-errors -I "$work" -I "$ffi" -module-name App \
        "$work/swift"/*.swift "$plugin"/native/ios/*.swift
fi
echo "$1 native backends compile against the generated code"
