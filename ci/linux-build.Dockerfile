# syntax=docker/dockerfile:1.7
FROM rust:1.95-bookworm

ENV CARGO_BUILD_JOBS=1 \
    CARGO_INCREMENTAL=0 \
    RUST_MIN_STACK=16777216

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        appstream \
        dbus-daemon \
        desktop-file-utils \
        file \
        jq \
        libacl1-dev \
        libarchive-dev \
        libfontconfig1-dev \
        libfreetype6-dev \
        libsmbclient-dev \
        libxcb1-dev \
        libxkbcommon-dev \
        libxkbcommon-x11-dev \
        librsvg2-bin \
        pkg-config \
        python3 \
    && rm -rf /var/lib/apt/lists/*

RUN rustup component add --toolchain 1.95.0 clippy rustfmt
RUN useradd --create-home musheen-test

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
    chown -R musheen-test:musheen-test \
        /usr/local/cargo/registry /usr/local/cargo/git /workspace/target \
    && runuser --user musheen-test -- \
        env CARGO_HOME=/usr/local/cargo cargo test --workspace --all-features --locked
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/workspace/target \
    cargo build --workspace --all-features --locked
RUN --mount=type=cache,target=/workspace/target \
    MUSHEEN_APP_BINARY=/workspace/target/debug/musheen \
    MUSHEEN_BROKER_BINARY=/workspace/target/debug/musheen-broker \
    scripts/package-smoke-test.sh
