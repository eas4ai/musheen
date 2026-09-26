#!/usr/bin/env bash
set -euo pipefail

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
scratch_base=${MUSHEEN_SCRATCH_BASE:-/tmp}
if [[ ! -d "$scratch_base" ]]; then
    echo "package scratch base does not exist: $scratch_base" >&2
    exit 1
fi

runtime_dir=${XDG_RUNTIME_DIR:-/tmp}
exec 9>"$runtime_dir/musheen-linux-build.lock"
if ! flock -n 9; then
    echo "another Musheen Docker verification is already running" >&2
    exit 1
fi

output_dir=$(mktemp -d -p "$scratch_base" musheen-arch-package-XXXXXX)
cleanup_on_failure() {
    if (( $? != 0 )); then
        rm -r -- "$output_dir"
    fi
}
trap cleanup_on_failure EXIT

docker build --pull=false \
    --file "$repository_root/ci/arch-package.Dockerfile" \
    --target artifact \
    --output "type=local,dest=$output_dir" \
    "$repository_root"

package=$(find "$output_dir" -maxdepth 1 -name 'musheen-*.pkg.tar.zst' -print -quit)
if [[ -z "$package" ]]; then
    echo "Arch build did not export a package" >&2
    exit 1
fi
printf 'Arch package: %s\n' "$package"
