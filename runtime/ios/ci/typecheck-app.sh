#!/bin/sh
# Type-check an example's iOS Swift sources against the iOS Simulator SDK
# without Xcode projects: the `IstmoRuntime` module, the code `istmo-build`
# generates into the app (dispatchers, codecs, registry, entry point) and
# the `native/ios` sources of every plugin the app links. macOS only.
#
# usage: runtime/ios/ci/typecheck-app.sh <example>
#   e.g. runtime/ios/ci/typecheck-app.sh data-store-demo
set -eu

root=$(cd "$(dirname "$0")/../../.." && pwd)
example=$1
ios="$root/examples/$example/ios"
target=${ISTMO_SWIFT_TARGET:-arm64-apple-ios16.0-simulator}
sdk=$(xcrun --sdk iphonesimulator --show-sdk-path)
ffi="$root/runtime/ios/Sources/IstmoFFI/include"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The crate's build script writes the generated Swift and the
# `istmo-plugins.yml` naming the plugin sources.
cargo check --quiet -p "$example"

plugins_yml=$(find "$ios" -mindepth 2 -maxdepth 2 -name istmo-plugins.yml | head -n 1)
if [ -z "$plugins_yml" ]; then
    echo "no istmo-plugins.yml under $ios" >&2
    exit 1
fi
app_dir=$(dirname "$plugins_yml")

swiftc -emit-module -parse-as-library \
    -sdk "$sdk" -target "$target" \
    -I "$ffi" \
    -module-name IstmoRuntime \
    -emit-module-path "$work/IstmoRuntime.swiftmodule" \
    "$root"/runtime/ios/Sources/IstmoRuntime/*.swift

# Plugin source groups are `- path: "<dir>"` entries relative to the app dir.
: > "$work/sources"
find "$app_dir" -name '*.swift' >> "$work/sources"
sed -n 's/^ *- path: "\(.*\)"$/\1/p' "$plugins_yml" | while read -r rel; do
    find "$app_dir/$rel" -name '*.swift' >> "$work/sources"
done

echo "type-checking $(wc -l < "$work/sources" | tr -d ' ') files of $example"
# shellcheck disable=SC2046 # one path per line, no spaces in the repo.
swiftc -typecheck \
    -sdk "$sdk" -target "$target" \
    -I "$work" -I "$ffi" \
    -module-name App \
    $(cat "$work/sources")
