# syntax=docker/dockerfile:1.7
FROM rust:1.95-bookworm AS verify

ENV CARGO_BUILD_JOBS=8 \
    CARGO_INCREMENTAL=0 \
    RUST_TEST_THREADS=1 \
    RUST_MIN_STACK=16777216

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        appstream dbus-daemon desktop-file-utils file git jq \
        libacl1-dev libarchive-dev libfontconfig1-dev libfreetype6-dev \
        libsmbclient-dev libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev \
        librsvg2-bin pkg-config python3 \
    && rm -rf /var/lib/apt/lists/*

RUN rustup toolchain install stable --profile minimal --component clippy,rustfmt
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo install cargo-deny --locked --version 0.20.2 --jobs 8

WORKDIR /workspace
COPY . .

RUN scripts/verify-msrv.sh /workspace \
    && scripts/verify-dependencies.sh /workspace \
    && cargo +1.95.0 fmt --all --check

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/workspace/target \
    scripts/verify-release-matrix.sh /workspace

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    python3 scripts/generate-sbom.py /workspace /release

FROM scratch AS artifact
COPY --from=verify /release/ /
