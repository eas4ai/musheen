#!/usr/bin/env bash
set -euo pipefail

repository_root=${1:-$(git rev-parse --show-toplevel)}
manifest="$repository_root/Cargo.toml"
lockfile="$repository_root/Cargo.lock"
toolchain="$repository_root/rust-toolchain.toml"

if ! grep -Eq '^[[:space:]]*channel[[:space:]]*=[[:space:]]*"1\.95\.0"[[:space:]]*$' "$toolchain"; then
    echo "rust-toolchain.toml must pin Rust 1.95.0" >&2
    exit 1
fi

before=$(sha256sum "$lockfile" | cut -d ' ' -f 1)
metadata=$(cargo metadata --manifest-path "$manifest" --format-version 1 --no-deps --locked)
after=$(sha256sum "$lockfile" | cut -d ' ' -f 1)
if [[ "$before" != "$after" ]]; then
    echo "Cargo.lock changed during locked metadata verification" >&2
    exit 1
fi

if ! jq -e --arg expected '1.95' '
    [.packages[] | select(.name == "musheen" or (.name | startswith("musheen-")))
        | {name, rust_version}] as $packages
    | ($packages | length > 0)
      and all($packages[]; .rust_version == $expected)
' <<< "$metadata" > /dev/null; then
    echo "every Musheen package must declare rust-version = 1.95" >&2
    exit 1
fi

echo "Musheen rust-version and locked dependency graph verified"
