#!/usr/bin/env bash
# Run the screen-capture plugin's direct Wayland backends (wlr-screencopy,
# ext-image-copy) against a headless sway with two outputs of known
# solid colours: HEADLESS-1 1280x720 at scale 1 (#336699), HEADLESS-2
# 1280x720 at scale 2 turned 90° (#cc3300) to its right.
#
# usage: scripts/test-screen-capture-sway.sh [cargo test args...]
#   WLR_RENDERER       pixman (default, shm only) or gles2 (DMA-BUF, needs a GPU)
#   CARGO_FEATURES     plugin features to build (default: the two backends)
#
# sway and swaybg come from nixpkgs when they are not on PATH; the
# compositor runs in a private XDG_RUNTIME_DIR and is always killed on
# exit.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
if ! command -v sway >/dev/null || ! command -v swaybg >/dev/null; then
    exec nix shell nixpkgs#sway nixpkgs#swaybg --command "$0" "$@"
fi

runtime=$(mktemp -d "${TMPDIR:-/tmp}/istmo-sc-sway.XXXXXX")
chmod 700 "$runtime"
sway_pid=
cleanup() {
    if [ -n "$sway_pid" ]; then
        kill "$sway_pid" 2>/dev/null || true
        wait "$sway_pid" 2>/dev/null || true
    fi
    rm -rf "$runtime"
}
trap cleanup EXIT INT TERM

# Nothing of the caller's desktop may leak into the test session.
unset WAYLAND_DISPLAY DISPLAY SWAYSOCK NIRI_SOCKET I3SOCK
export XDG_RUNTIME_DIR=$runtime
export XDG_CURRENT_DESKTOP=sway
export WLR_BACKENDS=headless
export WLR_LIBINPUT_NO_DEVICES=1
export WLR_HEADLESS_OUTPUTS=2
export WLR_RENDERER=${WLR_RENDERER:-pixman}

cat > "$runtime/config" <<'CONF'
output HEADLESS-1 mode 1280x720 position 0 0 scale 1 bg #336699 solid_color
output HEADLESS-2 mode 1280x720 position 1280 0 scale 2 transform 90 bg #cc3300 solid_color
default_border none
xwayland disable
CONF

sway -c "$runtime/config" >"$runtime/sway.log" 2>&1 &
sway_pid=$!

for _ in $(seq 100); do
    socket=$(find "$runtime" -maxdepth 1 -name 'wayland-*' ! -name '*.lock' -print -quit)
    [ -n "$socket" ] && break
    sleep 0.1
done
if [ -z "${socket:-}" ]; then
    cat "$runtime/sway.log" >&2
    echo "sway did not start" >&2
    exit 1
fi
export WAYLAND_DISPLAY=${socket##*/}
# The ext-image-copy test damages the output through sway IPC.
SWAYSOCK=$(find "$runtime" -maxdepth 1 -name "sway-ipc.*" -print -quit)
export SWAYSOCK
# Give swaybg time to paint both outputs.
sleep 1
echo "headless sway $(sway --version) on $WAYLAND_DISPLAY ($WLR_RENDERER)"

export ISTMO_SC_TEST_SWAY=1
# An idle headless output produces no damage: request full copies.
export ISTMO_SCREEN_CAPTURE_FULL_COPY=1
features=${CARGO_FEATURES:-linux-wlr-screencopy,linux-ext-image-copy}
cd "$root"
status=0
nix develop "$root" --command cargo test -p istmo-screen-capture --no-default-features \
    --features "$features" --test wayland_headless "$@" -- --nocapture --test-threads=1 || status=$?
if [ "$status" -ne 0 ]; then
    echo "--- sway log ---" >&2
    tail -n 40 "$runtime/sway.log" >&2
fi
exit "$status"
