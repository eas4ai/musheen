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
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY vendor/native-theme-gpui ./vendor/native-theme-gpui

RUN cargo build --locked
