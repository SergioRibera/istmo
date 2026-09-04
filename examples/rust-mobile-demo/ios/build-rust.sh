#!/bin/bash
# Invoked as an Xcode pre-build phase. Maps $PLATFORM_NAME + $ARCHS to a
# rust target triple, runs cargo, then stages the resulting .a into the
# LIBRARY_SEARCH_PATHS entry the linker expects.
#
# Xcode may pass multiple ARCHS for a device+simulator build; the loop
# handles each in isolation. Cargo does its own incremental work, so a
# no-op rebuild is fast.

set -euo pipefail

# When run outside Xcode (from CLI for debugging), synthesise the vars.
: "${PLATFORM_NAME:=iphonesimulator}"
: "${ARCHS:=arm64}"
: "${CONFIGURATION:=Debug}"
: "${PROJECT_DIR:=$(cd "$(dirname "$0")" && pwd)}"

# Workspace root: examples/rust-mobile-demo/ios → ../../..
WORKSPACE_DIR="$(cd "${PROJECT_DIR}/../../.." && pwd)"
STAGING_ROOT="${PROJECT_DIR}/build/rust"

profile_flag=""
profile_dir="debug"
if [[ "${CONFIGURATION}" == "Release" ]]; then
    profile_flag="--release"
    profile_dir="release"
fi

for arch in ${ARCHS}; do
    case "${PLATFORM_NAME}-${arch}" in
        iphoneos-arm64)         triple="aarch64-apple-ios" ;;
        iphonesimulator-arm64)  triple="aarch64-apple-ios-sim" ;;
        iphonesimulator-x86_64) triple="x86_64-apple-ios" ;;
        *)
            echo "unsupported PLATFORM_NAME/ARCH combination: ${PLATFORM_NAME}/${arch}" >&2
            exit 1
            ;;
    esac

    echo "cargo build --target ${triple} ${profile_flag}"
    cargo build \
        --manifest-path "${WORKSPACE_DIR}/Cargo.toml" \
        -p rust-mobile-demo \
        --target "${triple}" \
        ${profile_flag}

    src="${WORKSPACE_DIR}/target/${triple}/${profile_dir}/librust_mobile_demo.a"
    dst_dir="${STAGING_ROOT}/${PLATFORM_NAME}/${arch}"
    mkdir -p "${dst_dir}"
    cp "${src}" "${dst_dir}/librust_mobile_demo.a"
done
