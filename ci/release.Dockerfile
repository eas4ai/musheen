# syntax=docker/dockerfile:1.7
FROM rust:1.95-bookworm@sha256:6258907abe69656e41cd992e0b705cdcfabcbbe3db374f92ed2d47121282d4a1 AS verify

ENV CARGO_BUILD_JOBS=1 \
    CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_DEV_DEBUG=0 \
    CARGO_PROFILE_TEST_DEBUG=0 \
    RUST_TEST_THREADS=1 \
    RUST_MIN_STACK=16777216

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        appstream dbus-daemon desktop-file-utils file git jq \
        libacl1-dev libarchive-dev libfontconfig1-dev libfreetype6-dev \
        libsmbclient-dev libxcb1-dev libxkbcommon-dev libxkbcommon-x11-dev \
        librsvg2-bin pkg-config python3 x11-utils xauth xvfb \
    && rm -rf /var/lib/apt/lists/*

RUN rustup toolchain install stable --profile minimal --component clippy,rustfmt
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo install cargo-deny --locked --version 0.20.2 --jobs 1

WORKDIR /workspace
COPY . .

RUN scripts/verify-msrv.sh /workspace \
    && scripts/verify-dependencies.sh /workspace \
    && cargo +1.95.0 fmt --all --check

RUN useradd --create-home --uid 10001 musheen \
    && mkdir /release \
    && chown -R musheen:musheen /workspace /usr/local/cargo /release
USER musheen

RUN --mount=type=cache,target=/usr/local/cargo/registry,uid=10001,gid=10001 \
    --mount=type=cache,target=/usr/local/cargo/git,uid=10001,gid=10001 \
    --mount=type=cache,target=/workspace/target,uid=10001,gid=10001 \
    scripts/verify-release-matrix.sh /workspace \
    && CARGO_TARGET_DIR=/workspace/target scripts/check-budgets.sh \
    && CARGO_TARGET_DIR=/workspace/target scripts/run-benchmarks.sh

RUN --mount=type=cache,target=/usr/local/cargo/registry,uid=10001,gid=10001 \
    --mount=type=cache,target=/usr/local/cargo/git,uid=10001,gid=10001 \
    cargo fetch --locked \
    && python3 scripts/generate-sbom.py /workspace /release

FROM scratch AS artifact
COPY --from=verify /release/ /
