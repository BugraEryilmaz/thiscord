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
cargo build -p thiscord-backend --bin thiscord-backend --release --locked
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
if ! curl --silent --show-error --fail --max-time 20 https://thiscord.com.tr/api/v1/ready; then
    echo 'Public access check failed; healthy origin left running. Check DNS/Cloudflare.' >&2
    exit 1
fi
echo
echo "Previous release retained: $previous"
