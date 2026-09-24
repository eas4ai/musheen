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
    bash -euo pipefail -c '
        before=$(sha256sum Cargo.lock | cut -d " " -f 1)
        for toolchain in 1.95.0 stable; do
            for profile in debug release; do
                for features in minimal all; do
                    feature_args=(--no-default-features)
                    if [[ "$features" == all ]]; then
                        feature_args=(--all-features)
                    fi
                    profile_args=()
                    if [[ "$profile" == release ]]; then
                        profile_args=(--release)
                    fi
                    echo "Release matrix: Rust $toolchain, $features features, $profile"
                    cargo +"$toolchain" test --workspace --locked --jobs 8 \
                        "${feature_args[@]}" "${profile_args[@]}"
                done
            done
        done
        after=$(sha256sum Cargo.lock | cut -d " " -f 1)
        [[ "$before" == "$after" ]] || { echo "Cargo.lock changed during release matrix" >&2; exit 1; }
    '

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    python3 scripts/generate-sbom.py /workspace /release

FROM scratch AS artifact
COPY --from=verify /release/ /
