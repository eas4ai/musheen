# syntax=docker/dockerfile:1.7
FROM rust:1.95-bookworm

ENV CARGO_BUILD_JOBS=1 \
    RUST_MIN_STACK=16777216

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        libacl1-dev \
        libfontconfig1-dev \
        libfreetype6-dev \
        libxcb1-dev \
        libxkbcommon-dev \
        libxkbcommon-x11-dev \
    && rm -rf /var/lib/apt/lists/*

RUN rustup component add --toolchain 1.95.0 clippy rustfmt

WORKDIR /workspace
COPY . .

RUN cargo fmt --all --check
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/workspace/target \
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/workspace/target \
    cargo test --workspace --all-features --locked
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/workspace/target \
    cargo build --workspace --all-features --locked
