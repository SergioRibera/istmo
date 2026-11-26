#!/bin/sh
# Run the Kotlin and Swift `DeepLink` encoders of the native backends and
# compare their bytes with `tests/golden/deep_link.hex.expected` (written
# from Rust by `tests/deep_link_wire.rs`).
#
# usage: plugins/launcher-actions/tests/native/check-wire.sh
#   KOTLINC / KOTLIN  Kotlin compiler / runner (default `kotlinc` / `kotlin`)
#   SWIFTC            Swift compiler with real Foundation (default `swiftc`)
#   SKIP_KOTLIN=1 / SKIP_SWIFT=1 to run one side only
set -eu

here=$(cd "$(dirname "$0")" && pwd)
plugin=$(cd "$here/../.." && pwd)
root=$(cd "$plugin/../.." && pwd)
expected="$plugin/tests/golden/deep_link.hex.expected"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

if [ "${SKIP_KOTLIN:-}" != 1 ]; then
    echo "kotlin encoder"
    ${KOTLINC:-kotlinc} -Werror -d "$work/kotlin" \
        "$root/runtime/android/src/main/java/dev/istmo/runtime/Bincode.kt" \
        "$plugin/native/android/LauncherActionLink.kt" "$here/WireMain.kt"
    ${KOTLIN:-kotlin} -cp "$work/kotlin" WireMainKt > "$work/kotlin.hex"
    diff -u "$expected" "$work/kotlin.hex"
fi

if [ "${SKIP_SWIFT:-}" != 1 ]; then
    echo "swift encoder"
    # One module, like an app: the runtime's Bincode next to the plugin
    # file, so the `import IstmoRuntime` is replaced by plain membership.
    mkdir -p "$work/swift"
    sed '/^import IstmoRuntime$/d' "$plugin/native/ios/LauncherActionLink.swift" > "$work/swift/LauncherActionLink.swift"
    # Off Apple platforms the runtime's Foundation stub (a working `Data`)
    # stands in for Foundation, as in `runtime/ios/ci/typecheck-goldens.sh`.
    stub=""
    if [ "$(uname)" != Darwin ]; then
        ${SWIFTC:-swiftc} -parse-as-library -emit-library -static -emit-module -module-name Foundation \
            -emit-module-path "$work/swift/Foundation.swiftmodule" -o "$work/swift/libFoundation.a" \
            "$root/runtime/ios/ci/FoundationStub.swift"
        stub="-I $work/swift -L $work/swift -lFoundation"
    fi
    # shellcheck disable=SC2086 # `stub` is a list of flags.
    ${SWIFTC:-swiftc} -warnings-as-errors $stub -o "$work/swift/wire" \
        "$root/runtime/ios/Sources/IstmoRuntime/Bincode.swift" \
        "$work/swift/LauncherActionLink.swift" "$here/main.swift"
    "$work/swift/wire" > "$work/swift.hex"
    diff -u "$expected" "$work/swift.hex"
fi
echo "native DeepLink encoders match"
