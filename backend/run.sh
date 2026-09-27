#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
source "$HOME/.cargo/env"
# Keep Linux artifacts separate from Windows Cargo/rust-analyzer output.
export CARGO_TARGET_DIR="$HOME/.cache/thiscord-target"
exec cargo run -p thiscord-backend --locked -- "$@"
