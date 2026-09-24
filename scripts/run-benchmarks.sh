#!/usr/bin/env bash
set -euo pipefail

: "${CARGO_TARGET_DIR:?set CARGO_TARGET_DIR to the Musheen target directory}"
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-1}
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
    local benchmark benchmark_temp output worker app
    benchmark=$(cargo bench --locked --no-run --bench "$name" --message-format=json \
        | jq -r --arg target "$name" 'select(.reason == "compiler-artifact" and .target.name == $target and .executable != null) | .executable')
    if [[ ! -x $benchmark ]]; then
        printf '%s benchmark executable is missing: %s\n' "$name" "$benchmark" >&2
        exit 1
    fi
    if [[ $name == thumbnail ]]; then
        worker=$(cargo build --locked --release --package musheen-desktop \
            --bin musheen-thumbnail-worker --message-format=json \
            | jq -r 'select(.reason == "compiler-artifact" and .target.name == "musheen-thumbnail-worker" and .executable != null) | .executable')
        if [[ ! -x $worker ]]; then
            printf 'thumbnail worker executable is missing: %s\n' "$worker" >&2
            exit 1
        fi
    fi
    if [[ $name == startup ]]; then
        app=$(cargo build --locked --release --package musheen --bin musheen \
            --message-format=json \
            | jq -r 'select(.reason == "compiler-artifact" and .target.name == "musheen" and .executable != null) | .executable')
        if [[ ! -x $app ]]; then
            printf 'startup app executable is missing: %s\n' "$app" >&2
            exit 1
        fi
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

    if [[ $name == startup ]]; then
        output=$(TMPDIR="$benchmark_temp" MUSHEEN_STARTUP_APP="$app" \
            dbus-run-session -- xvfb-run -a "$benchmark" --bench)
    else
        output=$(TMPDIR="$benchmark_temp" MUSHEEN_THUMBNAIL_WORKER="${worker:-}" "$benchmark" --bench)
    fi
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
run_benchmark thumbnail
run_benchmark archive
run_benchmark terminal
run_benchmark remote
run_benchmark startup
