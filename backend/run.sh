#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
source "$HOME/.cargo/env"
# Keep Linux artifacts separate from Windows Cargo/rust-analyzer output.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/thiscord-target}"
cargo build -p thiscord-frontend --features voice-bot --bin thiscord-voice-bot --locked
export THISCORD_VOICE_BOT_PATH="$CARGO_TARGET_DIR/debug/thiscord-voice-bot"
exec cargo run -p thiscord-backend --locked -- "$@"
