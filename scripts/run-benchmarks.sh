#!/usr/bin/env bash
set -euo pipefail

: "${CARGO_TARGET_DIR:?set CARGO_TARGET_DIR to the Musheen target directory}"
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-4}
export CARGO_INCREMENTAL=0

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repository_root"

temporary_parent=${MUSHEEN_BENCH_TMP_PARENT:-/tmp}
if [[ ! -d $temporary_parent ]]; then
    printf 'benchmark temp parent is missing: %s\n' "$temporary_parent" >&2
    exit 1
fi

benchmark=$(cargo bench --locked --no-run --bench directory --message-format=json \
    | jq -r 'select(.reason == "compiler-artifact" and .target.name == "directory" and .executable != null) | .executable')
if [[ ! -x $benchmark ]]; then
    printf 'directory benchmark executable is missing: %s\n' "$benchmark" >&2
    exit 1
fi

benchmark_temp=$(mktemp -d -p "$temporary_parent" musheen-bench.XXXXXX)
cleanup() {
    local status=$?
    if ! rmdir "$benchmark_temp"; then
        printf 'benchmark left temporary files in %s\n' "$benchmark_temp" >&2
        status=1
    fi
    exit "$status"
}
trap cleanup EXIT

output=$(TMPDIR="$benchmark_temp" "$benchmark" --bench)
printf '%s\n' "$output"
if ! printf '%s\n' "$output" | jq -e -s '
    length == 3
    and (map(.case) | sort == ["first_directory_page", "million_item_directory_enumeration", "million_item_directory_scroll"])
    and all(.[];
        (.wall_ns | type) == "number" and .wall_ns >= 0
        and (.cpu_ns | type) == "number" and .cpu_ns >= 0
        and (.peak_rss_kib | type) == "number" and .peak_rss_kib > 0
        and ((.open_fds // .open_fds_sampled_max) | type) == "number"
        and (.queued_pages_max | type) == "number"
        and ((.temporary_bytes // .temporary_bytes_sampled_max) | type) == "number"
        and .retained_models_max <= 4096)
    and any(.[]; .case == "first_directory_page" and .items == 512 and .queued_pages_max <= 2 and (.temporary_bytes | type) == "number")
    and any(.[]; .case == "million_item_directory_enumeration" and .items == 1000000 and .pages == 1954 and .queued_pages_max <= 2 and (.temporary_bytes_sampled_max | type) == "number")
    and any(.[]; .case == "million_item_directory_scroll" and .items == 1000000 and .viewports >= 100 and (.temporary_bytes | type) == "number")
' >/dev/null; then
    printf 'directory benchmark did not emit complete bounded measurements\n' >&2
    exit 1
fi
