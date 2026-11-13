#!/bin/sh
# Compile the Kotlin that `istmo-build` generates into apps (the golden
# fixtures under `crates/build/tests/golden`) with warnings as errors, on
# a plain JVM: the runtime's JVM-only sources (`Bincode`, `PluginHandler`)
# plus the stubs in this directory stand in for the Android SDK.
#
# usage: runtime/android/ci/compile-goldens.sh
#   KOTLINC           compiler command (default `kotlinc`)
#   KOTLIN_CLASSPATH  extra classpath, at least kotlinx-coroutines-core-jvm
set -eu

root=$(cd "$(dirname "$0")/../../.." && pwd)
ci="$root/runtime/android/ci"
runtime="$root/runtime/android/src/main/java/dev/istmo/runtime"
golden="$root/crates/build/tests/golden"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

compile() {
    label=$1
    shift
    echo "compiling $label"
    # shellcheck disable=SC2086 # KOTLINC may carry its own arguments.
    ${KOTLINC:-kotlinc} -Werror ${KOTLIN_CLASSPATH:+-classpath "$KOTLIN_CLASSPATH"} \
        -d "$work/out-$label" \
        "$runtime/Bincode.kt" "$runtime/PluginHandler.kt" "$ci"/*.kt "$@"
}

# Generated files carry no `package` line: the app build prepends it.
stage() {
    dir=$1
    name=$2
    mkdir -p "$dir"
    { echo "package dev.istmo.runtime"; echo; cat "$golden/$name.kt.expected"; } > "$dir/$name.kt"
}

# Contracts whose types, codecs and both roles all have goldens.
for contract in coverage live_activity permissions; do
    host="$work/$contract-host"
    for part in types codecs host; do
        stage "$host" "${contract}_$part"
    done
    compile "$contract-host" "$host"/*.kt
    [ -f "$golden/${contract}_client.kt.expected" ] || continue
    client="$work/$contract-client"
    # The `<T>Codecs` interface: its own file in a client-only app, else
    # the dispatcher file declares it.
    interface=codecs_interface
    [ -f "$golden/${contract}_$interface.kt.expected" ] || interface=host
    for part in types codecs "$interface" client; do
        stage "$client" "${contract}_$part"
    done
    compile "$contract-client" "$client"/*.kt
done

# Services and workers declare their own package.
mkdir "$work/background"
for name in service_foreground worker_backup; do
    cp "$golden/$name.kt.expected" "$work/background/$name.kt"
done
compile background "$work/background"/*.kt
echo "generated Kotlin compiles without warnings"
