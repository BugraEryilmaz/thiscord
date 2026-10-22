#!/usr/bin/env bash
# Run as ubuntu, not root. Production credentials are loaded only by systemd.
set -Eeuo pipefail
umask 077

usage() {
    echo 'Usage: thiscord-deploy [--build-only] [GIT_REF]'
    echo 'Default: fetch and deploy origin/main from /home/ubuntu/thiscord.'
}
build_only=false
if [[ ${1:-} == --help ]]; then usage; exit 0; fi
if [[ ${1:-} == --build-only ]]; then build_only=true; shift; fi
[[ $# -le 1 && ${1:-origin/main} != -* ]] || { usage >&2; exit 2; }
[[ $(id -un) == ubuntu ]] || { echo 'Run as ubuntu, without sudo.' >&2; exit 1; }
ref=${1:-origin/main}
repo=/home/ubuntu/thiscord
export PATH="/home/ubuntu/.cargo/bin:$PATH"
mkdir -p /home/ubuntu/.cache
exec 9>/home/ubuntu/.cache/thiscord-deploy.lock
flock -n 9 || { echo 'Another deployment is running.' >&2; exit 1; }
sudo -n true
git -C "$repo" fetch origin --tags
commit=$(git -C "$repo" rev-parse --verify --end-of-options "$ref^{commit}")
source_dir=$(mktemp -d /tmp/thiscord-deploy.XXXXXXXX)
stopped=false
activated=false
previous=''
release=''
cleanup() {
    result=$?
    trap - EXIT
    if [[ $result -ne 0 && $stopped == true ]]; then
        if [[ $activated == false ]]; then
            echo 'Deployment failed before activation; restarting the previous backend.' >&2
            sudo -n systemctl start thiscord || true
        else
            sudo -n systemctl stop thiscord || true
            echo "Deployment failed after activation. Backend stopped; migrations may have run." >&2
            echo "Previous release: $previous; attempted release: $release" >&2
            echo 'Inspect sudo journalctl -u thiscord. Do not roll back the binary without checking migrations.' >&2
        fi
    fi
    case "$source_dir" in /tmp/thiscord-deploy.*) rm -rf -- "$source_dir" ;; esac
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
echo "Building $commit ($ref); the running backend stays available."
git -C "$repo" archive "$commit" | tar -x -C "$source_dir"
cd "$source_dir"
export CARGO_TARGET_DIR="$repo/target"
export CARGO_BUILD_JOBS=2 CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=8
[[ $(trunk --version) == 'trunk 0.21.14' ]] || { echo 'Install trunk 0.21.14 with cargo install trunk --version 0.21.14 --locked.' >&2; exit 1; }
pkg-config --exists alsa || { echo 'Install libasound2-dev for the native voice-bot companion before deploying.' >&2; exit 1; }
cargo build -p thiscord-backend --bin thiscord-backend --release --locked
cargo build -p thiscord-frontend --features voice-bot --bin thiscord-voice-bot --release --locked
# Validate the companion before touching the running release. CPAL links ALSA
# even though the bot uses virtual devices and never opens sound hardware.
ldd "$CARGO_TARGET_DIR/release/thiscord-voice-bot" > "$source_dir/voice-bot-libraries.txt"
if grep -q 'not found' "$source_dir/voice-bot-libraries.txt"; then
    echo 'Voice-bot companion has missing runtime libraries; deployment aborted.' >&2
    exit 1
fi
(cd frontend && trunk build --release --locked --public-url /admin/ --dist dist-admin)
[[ -s frontend/dist-admin/index.html ]] || { echo 'Admin bundle is missing; refusing to activate an API-only release.' >&2; exit 1; }
(cd frontend && trunk build welcome/index.html --release --locked --public-url /welcome/ --dist dist-welcome)
[[ -s frontend/dist-welcome/index.html ]] || { echo 'Welcome bundle is missing; deployment aborted.' >&2; exit 1; }
if [[ $build_only == true ]]; then
    echo "Build verified: $commit. No production service or database changes."
    exit 0
fi
sudo -n systemctl is-active --quiet thiscord
previous=$(sudo -n readlink -f /opt/thiscord/current)
[[ -x "$previous/thiscord-backend" ]] || { echo 'Previous release is invalid.' >&2; exit 1; }
release=$(sudo -n mktemp -d "/opt/thiscord/releases/${commit:0:12}-$(date -u +%Y%m%dT%H%M%SZ).XXXXXXXX")
sudo -n chmod 0755 "$release"
sudo -n install -o root -g root -m 0755 "$CARGO_TARGET_DIR/release/thiscord-backend" "$release/thiscord-backend"
sudo -n install -o root -g root -m 0755 "$CARGO_TARGET_DIR/release/thiscord-voice-bot" "$release/thiscord-voice-bot"
sudo -n install -d -o root -g root -m 0755 "$release/admin"
sudo -n cp -R frontend/dist-admin/. "$release/admin/"
sudo -n chown -R root:root "$release/admin"
sudo -n find "$release/admin" -type d -exec chmod 0755 {} +
sudo -n find "$release/admin" -type f -exec chmod 0644 {} +
sudo -n install -d -o root -g root -m 0755 "$release/welcome"
sudo -n cp -R frontend/dist-welcome/. "$release/welcome/"
sudo -n chown -R root:root "$release/welcome"
sudo -n find "$release/welcome" -type d -exec chmod 0755 {} +
sudo -n find "$release/welcome" -type f -exec chmod 0644 {} +
echo "Stopping backend and backing up PostgreSQL before activation: $release"
stopped=true
sudo -n systemctl stop thiscord
sudo -n systemctl start thiscord-backup.service
sudo -n systemctl is-failed --quiet thiscord-backup.service && exit 1
sudo -n ln -s "$release" "$release/current-link"
# Mark before swapping so an interrupted activation never triggers blind rollback.
activated=true
sudo -n mv -Tf "$release/current-link" /opt/thiscord/current
sudo -n systemctl start thiscord
ready=false
for ((attempt=0; attempt<60; attempt++)); do
    if sudo -n systemctl is-active --quiet thiscord &&
       curl --noproxy '*' --silent --fail --max-time 3 \
         --resolve thiscord.com.tr:443:127.0.0.1 \
         https://thiscord.com.tr/api/v1/ready >/dev/null; then
        ready=true
        break
    fi
    sleep 2
done
[[ $ready == true ]] || { echo 'Origin readiness check failed.' >&2; exit 1; }
stopped=false
echo "Deployed $commit. Origin HTTPS and database readiness passed."
if ! curl --noproxy '*' --silent --show-error --fail --max-time 10 \
    --resolve thiscord.com.tr:443:127.0.0.1 \
    https://thiscord.com.tr/admin >/dev/null; then
    echo 'Admin route check failed; healthy API left running. Check the installed bundle and ADMIN_DIST_DIR.' >&2
    exit 1
fi
if ! curl --silent --show-error --fail --max-time 20 https://thiscord.com.tr/api/v1/ready; then
    echo 'Public access check failed; healthy origin left running. Check DNS/Cloudflare.' >&2
    exit 1
fi
if ! curl --silent --show-error --fail --max-time 20 https://thiscord.com.tr/admin >/dev/null; then
    echo 'Public admin check failed; healthy origin left running. Check Cloudflare routing.' >&2
    exit 1
fi
echo
echo "Previous release retained: $previous"
