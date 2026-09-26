#!/usr/bin/env bash
set -euo pipefail

: "${CARGO_TARGET_DIR:?set CARGO_TARGET_DIR to the Musheen target directory}"
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-1}
export CARGO_INCREMENTAL=0

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repository_root"

run_case() {
    local requirement=$1 package=$2 suite=$3 filter=$4
    local output
    shift 4
    if ! output=$(cargo test --locked -p "$package" --test "$suite" "$filter" -- "$@" 2>&1); then
        printf '%s\n' "$output" >&2
        return 1
    fi
    printf '%s\n' "$output"
    if [[ ! $output =~ test\ result:\ ok\.\ [1-9][0-9]*\ passed ]]; then
        printf '%s: no budget test ran: %s\n' "$requirement" "$filter" >&2
        return 1
    fi
}

run_case LIMIT-001 musheen-desktop settings operations_capture_an_immutable_resource_limit_snapshot
run_case LIMIT-002 musheen-core limits default_directory_limits_match_the_production_budget
run_case LIMIT-002 musheen-core limits zero_and_above_maximum_limits_are_rejected
run_case LIMIT-002 musheen-core limits directory_retention_hard_max_matches_the_resident_model_cap
run_case LIMIT-003 musheen-ui search million_result_producer_requests_refinement_without_unbounded_models
run_case LIMIT-003 musheen-local search cancellation_interrupts_a_stalled_search_consumer
run_case LIMIT-004 musheen-desktop preview selection_reads_only_the_initial_mebibyte_of_a_sparse_tibibyte_file
run_case LIMIT-004 musheen-desktop preview preview_limits_reject_values_above_the_read_and_retention_budgets
run_case LIMIT-005 musheen-desktop thumbnail thumbnail_limits_reject_work_above_the_documented_budget
run_case LIMIT-005 musheen-desktop thumbnail default_pool_runs_no_more_than_four_workers
run_case LIMIT-006 musheen-desktop archive_operations default_archive_staging_budget_does_not_exceed_ten_gib
run_case LIMIT-006 musheen-desktop archive_browse compressed_tar_stream_enforces_expanded_limit_before_an_entry
run_case LIMIT-006 musheen-desktop archive_browse compression_ratio_limit_applies_while_browsing
run_case LIMIT-007 musheen-desktop terminal scrollback_drops_oldest_complete_lines_at_both_limits
run_case LIMIT-007 musheen-desktop terminal pty_output_backpressures_a_slow_consumer_without_losing_bytes
run_case LIMIT-008 musheen-desktop remote_pool defaults_are_fifteen_second_connect_sixty_second_idle_four_by_eight
run_case LIMIT-008 musheen-desktop remote_pool configured_pool_rejects_values_above_the_documented_budgets
run_case LIMIT-008 musheen-desktop remote_pool pool_caps_capacity_cancels_waiters_and_reconnects_discarded_connections
run_case LIMIT-009 musheen-ops scheduler defaults_provider_limits_and_fifo_progress_are_enforced
run_case LIMIT-002 musheen-ui shell streaming_directory_model_pages_through_one_million_items --ignored
