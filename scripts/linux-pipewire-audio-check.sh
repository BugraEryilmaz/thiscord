#!/usr/bin/env bash
# Runtime check of Linux shared system audio against a live PipeWire graph.
# Starts a private PipeWire + WirePlumber (own runtime and state directories, so
# a desktop session is untouched), a null sink as the graph driver and a pw-play
# tone, then runs the ignored capture test three times. It prints captured
# samples, peak level, links and consumer wakes (the test fails above 250 wakes
# in 2 s, i.e. if the consumer polls instead of being woken per buffer).
# Needs pipewire, wireplumber, pw-cli/pw-play and gst-launch-1.0. Run from the repo root.
set -u
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/thiscord-target}"
cargo test -p thiscord-frontend --lib --features screen-share --locked --no-run >/dev/null
export PIPEWIRE_RUNTIME_DIR=/tmp/thiscord-pw-check
state=/tmp/thiscord-wp-state
rm -rf "$PIPEWIRE_RUNTIME_DIR" "$state"
mkdir -p "$PIPEWIRE_RUNTIME_DIR"
pipewire >/dev/null 2>&1 &
pw=$!
sleep 1
XDG_STATE_HOME="$state" wireplumber >/dev/null 2>&1 &
wp=$!
sleep 1.5
pw-cli create-node adapter "{ factory.name=support.null-audio-sink node.name=thiscord-null \
  media.class=Audio/Sink object.linger=true audio.position=[FL FR] }" >/dev/null
tone=/tmp/thiscord-pw-tone.wav
[ -f "$tone" ] || gst-launch-1.0 -q audiotestsrc num-buffers=1500 wave=sine volume=0.3 \
  ! audio/x-raw,rate=48000,channels=2 ! wavenc ! filesink location="$tone"
for _ in 1 2 3; do
  timeout 15 pw-play "$tone" >/dev/null 2>&1 &
  play=$!
  sleep 1
  cargo test -p thiscord-frontend --lib --features screen-share --locked \
    captures_other_applications_from_a_live_pipewire_session -- --ignored --nocapture 2>&1 \
    | grep -E "captured|test result"
  kill "$play" 2>/dev/null
  wait "$play" 2>/dev/null
done
kill "$wp" "$pw"
wait "$wp" "$pw" 2>/dev/null
rm -rf "$PIPEWIRE_RUNTIME_DIR" "$state"
