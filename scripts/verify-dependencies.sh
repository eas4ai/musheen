#!/usr/bin/env bash
set -euo pipefail

repository_root=${1:-$(git rev-parse --show-toplevel)}
lockfile="$repository_root/Cargo.lock"
before=$(sha256sum "$lockfile" | cut -d ' ' -f 1)

python3 "$repository_root/scripts/verify-dependency-policy.py" "$repository_root"
cargo deny --locked \
    --manifest-path "$repository_root/Cargo.toml" \
    --config "$repository_root/deny.toml" \
    check advisories licenses sources

after=$(sha256sum "$lockfile" | cut -d ' ' -f 1)
if [[ "$before" != "$after" ]]; then
    echo "Cargo.lock changed during dependency verification" >&2
    exit 1
fi
