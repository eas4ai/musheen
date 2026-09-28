#!/usr/bin/env bash
set -euo pipefail

repository_root=$(git rev-parse --show-toplevel)
revision=$(git -C "$repository_root" rev-parse --verify HEAD)
runtime_dir=${XDG_RUNTIME_DIR:-/tmp}
lock_file="$runtime_dir/musheen-linux-build.lock"

exec 9>"$lock_file"
if ! flock -n 9; then
    echo "another Musheen Docker verification is already running" >&2
    exit 1
fi

git -C "$repository_root" archive --format=tar "$revision" \
    | docker build \
        --pull=false \
        --label "org.opencontainers.image.revision=$revision" \
        --file ci/linux-build.Dockerfile \
        --tag "musheen-linux-build:${revision:0:12}" \
        -
