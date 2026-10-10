#!/usr/bin/env bash
set -euo pipefail
root=$(git rev-parse --show-toplevel)
: "${CARGO_TARGET_DIR:?Select the CI Cargo cache before building}"
: "${RELEASE_TAG:?A release tag is required}"
docker info >/dev/null
toolchain=$(python3 -c 'import tomllib; print(tomllib.load(open("rust-toolchain.toml", "rb"))["toolchain"]["channel"])')
# Content-address the image so overlapping runner services cannot replace it.
image="thiscord-linux-installer:$(cat "$root/infra/ci/linux-installer.Dockerfile" "$root/rust-toolchain.toml" | sha256sum | cut -c1-32)"
docker build --build-arg "RUST_VERSION=$toolchain" -t "$image" -f "$root/infra/ci/linux-installer.Dockerfile" "$root/infra/ci"
mkdir -p "$CARGO_TARGET_DIR"
docker run --rm --init --user "$(id -u):$(id -g)" \
    --mount "type=bind,source=$root,target=/workspace" \
    --mount "type=bind,source=$CARGO_TARGET_DIR,target=/cache" \
    -e HOME=/tmp -e CARGO_HOME=/cache/cargo-home -e CARGO_TARGET_DIR=/cache/target \
    -e CARGO_BUILD_BUILD_DIR=/cache/target \
    -e "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}" \
    -e THISCORD_API_URL -e RELEASE_TAG -e GITHUB_REPOSITORY \
    -e TAURI_SIGNING_PRIVATE_KEY -e TAURI_SIGNING_PRIVATE_KEY_PASSWORD \
    "$image" pwsh -NoProfile -File /workspace/scripts/build-linux-installer.ps1
