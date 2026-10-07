FROM ubuntu:24.04@sha256:534baea6a22c03a63003dbc8dbe78fe34bc0d7e595d9a9dc9834884ff530eb55

ARG RUST_VERSION=1.98.1
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl git build-essential pkg-config cmake file xz-utils \
    libwebkit2gtk-4.1-dev libssl-dev libdbus-1-dev libxdo-dev \
    libayatana-appindicator3-dev librsvg2-dev patchelf libasound2-dev libfuse2t64 \
    && curl -fsSL https://packages.microsoft.com/config/ubuntu/24.04/packages-microsoft-prod.deb -o /tmp/microsoft.deb \
    && dpkg -i /tmp/microsoft.deb && rm /tmp/microsoft.deb \
    && apt-get update && apt-get install -y --no-install-recommends powershell \
    && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo
ENV PATH=/opt/tauri/bin:/opt/cargo/bin:$PATH
RUN curl -fsSL https://sh.rustup.rs -o /tmp/rustup.sh \
    && sh /tmp/rustup.sh -y --profile minimal --default-toolchain "$RUST_VERSION" \
    && rustup component add rustfmt clippy rust-analyzer \
    && rustup target add wasm32-unknown-unknown \
    && rm /tmp/rustup.sh \
    && chmod -R a+rX /opt/rustup /opt/cargo
RUN curl -fsSL https://github.com/tauri-apps/tauri/releases/download/tauri-cli-v2.12.0/cargo-tauri-x86_64-unknown-linux-gnu.tgz -o /tmp/tauri.tgz \
    && echo '8544e19c4312f653e80f92241ee4212b4d928ad33461bfb1d2997c52560c418f  /tmp/tauri.tgz' | sha256sum -c - \
    && mkdir -p /opt/tauri/bin && tar -xzf /tmp/tauri.tgz -C /opt/tauri/bin \
    && rm /tmp/tauri.tgz \
    && chmod -R a+rX /opt/tauri
# AppImage tools can extract themselves without privileged /dev/fuse access.
ENV APPIMAGE_EXTRACT_AND_RUN=1
WORKDIR /workspace
