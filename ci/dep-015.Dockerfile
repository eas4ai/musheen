FROM rust:1.95.0-slim-bookworm

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        build-essential \
        libfontconfig1-dev \
        libwayland-dev \
        libxkbcommon-x11-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace
ENV CARGO_BUILD_JOBS=1 \
    CARGO_INCREMENTAL=0 \
    RUST_MIN_STACK=16777216

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY vendor/native-theme-gpui ./vendor/native-theme-gpui

RUN --mount=type=cache,id=musheen-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=musheen-target,target=/workspace/target \
    cargo build --locked
