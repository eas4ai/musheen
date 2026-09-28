#!/usr/bin/env bash
set -euo pipefail

repository_root=$(git rev-parse --show-toplevel)
revision=$(git -C "$repository_root" rev-parse --verify HEAD)
scratch_base=${MUSHEEN_SCRATCH_BASE:-/tmp}
if [[ ! -d "$scratch_base" ]]; then
    echo "release scratch base does not exist: $scratch_base" >&2
    exit 1
fi

runtime_dir=${XDG_RUNTIME_DIR:-/tmp}
exec 9>"$runtime_dir/musheen-linux-build.lock"
if ! flock -n 9; then
    echo "another Musheen Docker verification is already running" >&2
    exit 1
fi

output_dir=$(mktemp -d -p "$scratch_base" musheen-release-XXXXXX)
cleanup_on_failure() {
    if (( $? != 0 )); then
        rm -r -- "$output_dir"
    fi
}
trap cleanup_on_failure EXIT

git -C "$repository_root" archive --format=tar "$revision" \
    | docker build --pull=false --progress=plain \
        --label "org.opencontainers.image.revision=$revision" \
        --file ci/release.Dockerfile \
        --target artifact \
        --output "type=local,dest=$output_dir" \
        -

for artifact in musheen.cdx.json THIRD_PARTY_LICENSES.md; do
    if [[ ! -s "$output_dir/$artifact" ]]; then
        echo "release build did not export $artifact" >&2
        exit 1
    fi
done

printf 'Release artifacts: %s\n' "$output_dir"
