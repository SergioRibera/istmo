#!/bin/sh
# Type-check the Swift that `istmo-build` generates into apps (the golden
# fixtures under `crates/build/tests/golden`) with warnings as errors, on
# any host with a Swift toolchain. Stub `Foundation` / `BackgroundTasks`
# modules stand in for the Apple SDK; `IstmoRuntime` is the real runtime
# core built against them, as apps import it.
#
# usage: runtime/ios/ci/typecheck-goldens.sh [swiftc]
set -eu

root=$(cd "$(dirname "$0")/../../.." && pwd)
swiftc=${1:-swiftc}
ci="$root/runtime/ios/ci"
src="$root/runtime/ios/Sources/IstmoRuntime"
ffi="$root/runtime/ios/Sources/IstmoFFI/include"
golden="$root/crates/build/tests/golden"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

module() {
    name=$1
    shift
    "$swiftc" -parse-as-library -emit-module -module-name "$name" \
        -I "$work" -I "$ffi" -emit-module-path "$work/$name.swiftmodule" "$@"
}
module Foundation "$ci/FoundationStub.swift"
module BackgroundTasks "$ci/BackgroundTasksStub.swift"
module IstmoRuntime "$src/Bincode.swift" "$src/PluginException.swift" \
    "$src/PluginHandler.swift" "$src/IstmoRuntime.swift"

typecheck() {
    label=$1
    shift
    echo "type-checking $label"
    "$swiftc" -typecheck -warnings-as-errors -I "$work" -I "$ffi" -module-name App "$@"
}

# Contracts whose types, codecs and both roles all have goldens.
for contract in coverage live_activity permissions; do
    for role in host client; do
        [ -f "$golden/${contract}_$role.swift.expected" ] || continue
        set_dir="$work/$contract-$role"
        mkdir "$set_dir"
        for part in types codecs "$role"; do
            cp "$golden/${contract}_$part.swift.expected" "$set_dir/${contract}_$part.swift"
        done
        typecheck "$contract $role" "$set_dir"/*.swift
    done
done

mkdir "$work/background"
for kind in processing refresh voip; do
    cp "$golden/ios_background_$kind.swift.expected" "$work/background/$kind.swift"
done
typecheck "background shims" "$work/background"/*.swift
echo "generated Swift type-checks without warnings"
