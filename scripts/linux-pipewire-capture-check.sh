#!/usr/bin/env bash
# Runtime check of Linux screen capture against a live PipeWire session (no
# desktop portal needed). Publishes a synthetic 1080p144 video node, then runs
# the ignored capture test twice: with the cadence gate before the copy (the
# shipped order) and with THISCORD_PIPEWIRE_GATE_AFTER_COPY=1 (the previous
# copy-then-throttle order), printing offered/admitted rates and process CPU.
# Needs pipewire, pw-cli and the GStreamer pipewire plugin. Run from the repo root.
set -u
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/thiscord-target}"
cargo test -p thiscord-frontend --lib --features screen-share --locked --no-run >/dev/null
run() {
  pkill -f thiscord-probe 2>/dev/null; sleep 1
  timeout 30 gst-launch-1.0 -q videotestsrc is-live=true pattern=ball \
    ! video/x-raw,format=BGRx,width=1920,height=1080,framerate=144/1 \
    ! pipewiresink mode=provide client-name=thiscord-probe \
      'stream-properties=p,media.class=Video/Source,node.name=thiscord-probe' >/dev/null 2>&1 &
  sleep 3
  node=$(pw-cli ls Node 2>/dev/null | grep -B6 'node.name = "thiscord-probe"' \
    | grep -o 'id [0-9]*' | head -1 | cut -d' ' -f2)
  env "$@" THISCORD_PIPEWIRE_NODE="$node" cargo test -p thiscord-frontend --lib \
    --features screen-share --locked live_pipewire_capture -- --ignored --nocapture 2>&1 \
    | grep -E "family=|test result"
}
run THISCORD_PIPEWIRE_GATE_BEFORE_COPY=1
run THISCORD_PIPEWIRE_GATE_AFTER_COPY=1
pkill -f thiscord-probe 2>/dev/null
