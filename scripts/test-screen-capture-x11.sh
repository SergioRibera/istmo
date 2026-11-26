#!/bin/sh
# Run the screen-capture X11 backend against headless Xvfb servers. The
# test itself splits the 640x480 screen into two RandR monitors (Xvfb
# drops monitors when the client that made them disconnects, so
# `xrandr --setmonitor` cannot do it). Three servers in turn: every
# extension, no MIT-SHM (socket GetImage), no DAMAGE (polling).
# Needs nix (Xvfb comes from nixpkgs) and the repo's dev shell for
# cargo. Xvfb is always stopped on exit.
#
# usage: scripts/test-screen-capture-x11.sh [extra cargo test args]
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
xvfb_pid=
stop_xvfb() {
    if [ -n "$xvfb_pid" ]; then
        kill "$xvfb_pid" 2>/dev/null || true
        wait "$xvfb_pid" 2>/dev/null || true
        xvfb_pid=
    fi
}
cleanup() {
    stop_xvfb
    rm -rf "$work"
}
trap cleanup EXIT INT TERM

xvfb=$(nix build --no-link --print-out-paths nixpkgs#xvfb)

run() {
    label=$1
    shift
    rm -f "$work/display"
    # Xvfb picks a free display and writes its number to fd 3.
    "$xvfb/bin/Xvfb" -displayfd 3 -screen 0 640x480x24 -nolisten tcp \
        +extension Composite +extension RANDR "$@" 3>"$work/display" 2>"$work/xvfb.log" &
    xvfb_pid=$!
    for _ in $(seq 100); do
        [ -s "$work/display" ] && break
        sleep 0.1
    done
    [ -s "$work/display" ] || { cat "$work/xvfb.log"; echo "Xvfb did not start"; exit 1; }
    display=":$(cat "$work/display")"
    echo "== $label (Xvfb on $display)"
    # An X11 session: no Wayland variables, so the backend is ready.
    (cd "$root" && env -u WAYLAND_DISPLAY XDG_SESSION_TYPE=x11 DISPLAY="$display" ISTMO_SC_TEST_X11=1 \
        CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}" \
        nix develop --command cargo test -p istmo-screen-capture --no-default-features --features linux-x11 \
        --test x11_xvfb ${CARGO_TEST_ARGS:-} -- --nocapture)
    stop_xvfb
}

run "all extensions"
run "no MIT-SHM" -extension MIT-SHM
run "no DAMAGE" -extension DAMAGE
echo "X11 backend passed under Xvfb"
