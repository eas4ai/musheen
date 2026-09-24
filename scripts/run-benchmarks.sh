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

run_benchmark() (
    local name=$1
    local benchmark benchmark_temp output
    benchmark=$(cargo bench --locked --no-run --bench "$name" --message-format=json \
        | jq -r --arg target "$name" 'select(.reason == "compiler-artifact" and .target.name == $target and .executable != null) | .executable')
    if [[ ! -x $benchmark ]]; then
        printf '%s benchmark executable is missing: %s\n' "$name" "$benchmark" >&2
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
    if ! printf '%s\n' "$output" | jq -e -s --arg benchmark "$name" \
        -f scripts/benchmark-validations.jq >/dev/null; then
        printf '%s benchmark did not emit complete bounded measurements\n' "$name" >&2
        exit 1
    fi
)

run_benchmark directory
run_benchmark search
run_benchmark operations
