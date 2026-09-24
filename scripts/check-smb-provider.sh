#!/usr/bin/env bash
set -euo pipefail

repository=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
image=musheen-smb-check:local
container=musheen-smb-check
docker_config=${DOCKER_CONFIG:-/home/shawn/.config/docker-hub/config}

DOCKER_CONFIG="$docker_config" docker build \
    --file "$repository/ci/smb-check.Dockerfile" \
    --tag "$image" \
    "$repository"

DOCKER_CONFIG="$docker_config" docker run --rm \
    --name "$container" \
    --volume "$repository:/work:ro" \
    --workdir /work \
    --env CARGO_BUILD_JOBS=1 \
    --env CARGO_TARGET_DIR=/tmp/musheen-target \
    "$image" \
    scripts/check-smb-provider-container.sh
