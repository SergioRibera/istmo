#!/bin/sh
# Runs the plain-Foundation parts of the iOS backend of
# istmo.notifications on any host with a Swift toolchain and the real
# swift-corelibs-foundation (TimeZone data included), without an SDK:
# the calendar rule against tests/schedule_vectors.tsv, trigger timing,
# missed deliveries, the store's arming plan and the validation port,
# with the generated Swift types from tests/golden/notifications_types.
# The rest of native/ios is type-checked against SDK stubs by
# scripts/check-plugin-native.sh.
#
# usage: plugins/notifications/tests/native/ios/check.sh
#   SWIFTC       compiler (default `swiftc`)
#   SWIFT_RUN    prefix for running the binary, e.g. a bwrap call that
#                binds tzdata at /usr/share/zoneinfo (corelibs Foundation
#                reads zones only there)
set -eu

here=$(cd "$(dirname "$0")" && pwd)
plugin=$(cd "$here/../../.." && pwd)
native="$plugin/native/ios"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cp "$plugin/tests/golden/notifications_types.swift.expected" "$work/notifications_types.swift"
echo "swift: calendar rule vectors, trigger timing, missed deliveries, arming plan, validation"
${SWIFTC:-swiftc} -warnings-as-errors -module-name Checks -o "$work/checks" \
    "$work/notifications_types.swift" "$native/NotificationsCalendar.swift" "$native/NotificationsValidate.swift" \
    "$here/main.swift"
# shellcheck disable=SC2086 # SWIFT_RUN may carry its own arguments.
${SWIFT_RUN:-} "$work/checks" "$plugin/tests/schedule_vectors.tsv"
