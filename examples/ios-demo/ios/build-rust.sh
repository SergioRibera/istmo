#!/bin/bash
# Invoked as an Xcode pre-build phase. Maps $PLATFORM_NAME + $ARCHS to a
# rust target triple, runs cargo, then stages the resulting .a into the
# LIBRARY_SEARCH_PATHS entry the linker expects.
#
# Xcode may pass multiple ARCHS for a device+simulator build; the loop
# handles each in isolation. Cargo does its own incremental work, so a
# no-op rebuild takes ~50 ms per arch.

set -euo pipefail

# When run outside Xcode (from CLI for debugging), synthesise the vars.
: "${PLATFORM_NAME:=iphonesimulator}"
: "${ARCHS:=arm64}"
: "${CONFIGURATION:=Debug}"
: "${PROJECT_DIR:=$(cd "$(dirname "$0")" && pwd)}"

# Workspace root: examples/ios-demo/ios → ../../..
WORKSPACE_DIR="$(cd "${PROJECT_DIR}/../../.." && pwd)"
STAGING_ROOT="${PROJECT_DIR}/build/rust"

profile_flag=""
profile_dir="debug"
if [[ "${CONFIGURATION}" == "Release" ]]; then
    profile_flag="--release"
    profile_dir="release"
fi

# Xcode passes ARCHS as a space-separated list.
for arch in ${ARCHS}; do
    case "${PLATFORM_NAME}-${arch}" in
        iphoneos-arm64)      triple="aarch64-apple-ios" ;;
        iphonesimulator-arm64) triple="aarch64-apple-ios-sim" ;;
        iphonesimulator-x86_64) triple="x86_64-apple-ios" ;;
        *)
            echo "unsupported PLATFORM_NAME/ARCH combination: ${PLATFORM_NAME}/${arch}" >&2
            exit 1
            ;;
    esac

    echo "cargo build --target ${triple} ${profile_flag}"
    cargo build \
        --manifest-path "${WORKSPACE_DIR}/Cargo.toml" \
        -p istmo-ios-demo \
        --target "${triple}" \
        ${profile_flag}

    src="${WORKSPACE_DIR}/target/${triple}/${profile_dir}/libistmo_ios_demo.a"
    dst_dir="${STAGING_ROOT}/${PLATFORM_NAME}/${arch}"
    mkdir -p "${dst_dir}"
    # Copy is cheap; symlink would break Xcode's timestamp-based
    # incremental link decisions on some setups.
    cp "${src}" "${dst_dir}/libistmo_ios_demo.a"
done
