#!/bin/sh
# Type-check the UIKit-free core of the Swift runtime (`IstmoRuntime`,
# `PluginHandler`, `PluginException`) on any host with a Swift toolchain,
# Linux included. `FoundationStub.swift` stands in for the small slice of
# Foundation / Dispatch these files use, so no Apple SDK is needed.
#
# usage: runtime/ios/ci/typecheck-stub.sh [swiftc]
set -eu

root=$(cd "$(dirname "$0")/../../.." && pwd)
swiftc=${1:-swiftc}
src="$root/runtime/ios/Sources/IstmoRuntime"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for f in IstmoRuntime PluginHandler PluginException; do
    grep -v '^import Foundation' "$src/$f.swift" > "$work/$f.swift"
done

"$swiftc" -typecheck \
    -I "$root/runtime/ios/Sources/IstmoFFI/include" \
    -module-name IstmoRuntime \
    "$root/runtime/ios/ci/FoundationStub.swift" \
    "$work/IstmoRuntime.swift" "$work/PluginHandler.swift" "$work/PluginException.swift"
echo "IstmoRuntime core type-checks"
