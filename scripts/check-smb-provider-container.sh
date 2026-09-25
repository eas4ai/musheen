#!/usr/bin/env bash
set -euo pipefail

cargo clippy -p musheen-desktop --features smb-pavao --tests --locked -- -D warnings
cargo test -p musheen-desktop --features smb-pavao \
    --test smb_contract --test nfs_contract --locked
