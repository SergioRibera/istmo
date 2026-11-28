#!/bin/sh
# Checks the Android backend of istmo.notifications without a device:
#
# 1. JVM: CalendarRule.kt against tests/schedule_vectors.tsv, Coalescer.kt
#    against the unit cases of src/coalesce.rs, trigger timing, missed
#    deliveries, the schedule store, validation and response deep links
#    (NativeChecks.kt), with the generated Kotlin from tests/golden/app_notifications_*.
# 2. Android: every native/android/*.kt compiled with -Werror against
#    android.jar (API 34, the compileSdk apps use) plus the AndroidX jars
#    the plugin's istmo.toml declares (fetched from Google Maven once).
#    Real SDK classes, not the JVM stubs of check-plugin-native.sh.
#
# usage: plugins/notifications/tests/native/android/check.sh
#   KOTLINC / KOTLIN    compiler / runner (default `kotlinc` / `kotlin`)
#   KOTLIN_CLASSPATH    at least kotlinx-coroutines-core-jvm
#   ANDROID_JAR         default $ANDROID_HOME/platforms/android-34/android.jar
#   ISTMO_ANDROIDX_CACHE  where the AndroidX jars go (default ~/.cache/istmo-androidx)
#   SKIP_ANDROID=1      only the JVM checks
set -eu

here=$(cd "$(dirname "$0")" && pwd)
plugin=$(cd "$here/../../.." && pwd)
root=$(cd "$plugin/../.." && pwd)
runtime="$root/runtime/android/src/main/java/dev/istmo/runtime"
golden="$plugin/tests/golden"
native="$plugin/native/android"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
kotlinc=${KOTLINC:-kotlinc}
cp=${KOTLIN_CLASSPATH:-}

mkdir "$work/gen"
for part in types codecs host; do
    cp "$golden/app_notifications_$part.kt.expected" "$work/gen/notifications_$part.kt"
done

echo "jvm: calendar rule vectors, coalescer cases, schedule engine and store"
# shellcheck disable=SC2086 # KOTLINC may carry its own arguments.
$kotlinc -Werror ${cp:+-classpath "$cp"} -d "$work/jvm" \
    "$runtime/Bincode.kt" "$runtime/PluginHandler.kt" "$work/gen"/*.kt \
    "$native/CalendarRule.kt" "$native/Coalescer.kt" "$native/ScheduleStore.kt" "$native/Validate.kt" "$native/ResponseDeepLink.kt" "$here/NativeChecks.kt"
${KOTLIN:-kotlin} -cp "$work/jvm${cp:+:$cp}" NativeChecksKt "$plugin/tests/schedule_vectors.tsv"

if [ -n "${SKIP_ANDROID:-}" ]; then
    exit 0
fi

android_jar=${ANDROID_JAR:-${ANDROID_HOME:-}/platforms/android-34/android.jar}
if [ ! -f "$android_jar" ]; then
    echo "android.jar not found ($android_jar): set ANDROID_JAR or ANDROID_HOME" >&2
    exit 1
fi
cache=${ISTMO_ANDROIDX_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/istmo-androidx}
mkdir -p "$cache"
maven=https://dl.google.com/android/maven2
# group/artifact/version/packaging: istmo.toml's androidx.core and
# androidx.activity, and what their public types extend.
deps="
androidx/core/core/1.13.1/aar
androidx/activity/activity/1.8.1/aar
androidx/lifecycle/lifecycle-common/2.6.1/jar
androidx/lifecycle/lifecycle-runtime/2.6.1/aar
androidx/lifecycle/lifecycle-viewmodel/2.6.1/aar
androidx/savedstate/savedstate/1.2.1/aar
androidx/annotation/annotation-jvm/1.6.0/jar
androidx/versionedparcelable/versionedparcelable/1.1.1/aar
"
classpath=$android_jar
for dep in $deps; do
    path=${dep%/*}
    kind=${dep##*/}
    version=${path##*/}
    artifact=${path%/*}
    artifact=${artifact##*/}
    jar="$cache/$artifact-$version.jar"
    if [ ! -f "$jar" ]; then
        echo "fetching $artifact $version"
        if [ "$kind" = aar ]; then
            curl -fsSL -o "$cache/$artifact-$version.aar" "$maven/$path/$artifact-$version.aar"
            unzip -p "$cache/$artifact-$version.aar" classes.jar > "$jar.tmp"
            rm "$cache/$artifact-$version.aar"
        else
            curl -fsSL -o "$jar.tmp" "$maven/$path/$artifact-$version.jar"
        fi
        mv "$jar.tmp" "$jar"
    fi
    classpath="$classpath:$jar"
done

echo "android: native/android against android.jar $(basename "$(dirname "$android_jar")") and AndroidX"
# The runtime first, without -Werror (its own gate is the Gradle build;
# IstmoActivities.kt needs the games SDK and is left out), then the
# generated code and the backend against it with warnings as errors.
runtime_sources=$(find "$runtime" -name "*.kt" ! -name IstmoActivities.kt | sort)
# shellcheck disable=SC2086
$kotlinc -nowarn -no-jdk -jvm-target 17 -classpath "$classpath${cp:+:$cp}" -d "$work/runtime" $runtime_sources
# shellcheck disable=SC2086
$kotlinc -Werror -no-jdk -jvm-target 17 -classpath "$classpath:$work/runtime${cp:+:$cp}" -d "$work/android" \
    "$work/gen"/*.kt "$native"/*.kt
echo "istmo.notifications android backend checks pass"
