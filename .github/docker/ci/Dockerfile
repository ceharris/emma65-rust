# CI image for emma65: system library dependencies needed to build the
# workspace's Tauri debugger and SDL2 peripheral binaries. The Rust toolchain
# itself is installed per-job by dtolnay/rust-toolchain, not baked in here, so
# jobs keep tracking "stable" without needing an image rebuild.
#
# Built and published by .github/workflows/docker-ci-image.yml.

FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    pkg-config \
    ca-certificates \
    curl \
    git \
    libssl-dev \
    libwebkit2gtk-4.1-dev \
    libayatana-appindicator3-dev \
    librsvg2-dev \
    libsdl2-dev \
    libsdl2-gfx-dev \
    && rm -rf /var/lib/apt/lists/*
