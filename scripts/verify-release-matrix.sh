#!/usr/bin/env bash
set -euo pipefail

repository_root=${1:-$(git rev-parse --show-toplevel)}
cd "$repository_root"

before=$(sha256sum Cargo.lock | cut -d ' ' -f 1)
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
after=$(sha256sum Cargo.lock | cut -d ' ' -f 1)
if [[ "$before" != "$after" ]]; then
    echo 'Cargo.lock changed during release matrix' >&2
    exit 1
fi
