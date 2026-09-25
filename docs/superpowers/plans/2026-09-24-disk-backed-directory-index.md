# Disk-backed Directory Index Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Browse and sort at least one million directory entries with no 100,000-item stop and no more than 4,096 resident item models.

**Architecture:** Keep the existing in-memory view for small folders. When a tab crosses the model limit, move its entries into a private temporary record file and externally sorted position files. GPUI requests viewport ranges through background work; stable IDs preserve selection while rows are evicted.

**Tech Stack:** Rust, `tempfile`, `serde_json`, GPUI background tasks, the existing `Store` paging and `DirectoryViewModel` comparators. No database crate or development GitHub CI.

**Design:** `docs/superpowers/specs/2026-09-24-disk-backed-directory-index-design.md`.

For every Cargo command below, set `CARGO_TARGET_DIR=/home/shawn/workspace2/scratchpads/musheen-target`, `CARGO_BUILD_JOBS=1`, and `CARGO_INCREMENTAL=0`. Run host builds with `ionice -c3 nice -n 10` to give other disk work priority.

---

## File boundaries

- Create `crates/musheen-ui/src/directory/index.rs`: private record codec, external sorter, ordered range reads, stable-ID lookup, and temporary-file lifetime. It owns no UI state.
- Modify `crates/musheen-ui/src/directory.rs`: tab generation, memory-to-disk spill, page progress, indexed counts, and stale-result guards.
- Modify `crates/musheen-ui/src/views/{mod,sort,group,columns}.rs` and `search/filters.rs`: share one comparison/filter policy and keep only a bounded resident window. Column parents must not retain every item.
- Modify `crates/musheen-ui/src/app.rs`: run index I/O in background tasks, feed provider pages without a 100,000-item cap, request visible ranges, and resolve off-screen actions by ID.
- Extend `crates/musheen-ui/tests/{shell,views,search,navigation}.rs` and focused `app.rs` tests. Keep the million-item fixture synthetic to avoid creating a million host files.
- Update `docs/spec/{browse,limits}.md` and the older browse plan where its selected-item exception conflicts with the approved absolute cap.

## Task 1: Private record file and lifetime

- [ ] Add a failing unit test in `directory/index.rs` for a record with a non-UTF-8 Unix path, a provider key, missing metadata, and a truncated length prefix. Assert exact round-trip bytes and an error for truncation.
- [ ] Run `cargo test --locked -p musheen-ui directory::index::tests::record_round_trip_and_truncation -- --exact`; expect failure because the index module does not exist.
- [ ] Add `mod index;` to `directory.rs`. Store a record as a checked `u32` length followed by JSON of this private DTO; reject lengths over the agreed maximum before allocating:

  ```rust
  #[derive(serde::Serialize, serde::Deserialize)]
  struct IndexRecord {
      id: ItemId,
      path: StorePath,
      display_name: String,
      kind: u8,
      size: Option<u64>,
      modified_unix_seconds: Option<i64>,
      arrival: u64,
  }
  ```

  Set `MAX_RECORD_BYTES` to 1 MiB. Convert `kind` with an explicit four-value match, never `as` on untrusted bytes. Use `tempfile::TempDir`; require owner-only directory and file permissions. Keep record offsets as `u64` and use checked seeks and sizes.
- [ ] Re-run the test; add a drop test that checks the private directory is removed after the last index owner drops. Commit `feat(ui): add private directory index records`.

## Task 2: Bounded global ordering and range reads

- [ ] Add a failing test in `directory/index.rs` that appends more than two 4,096-item runs in reverse arrival order, then checks first, middle, last, and backward ranges. Include `item-2`/`item-10`, duplicate comparison keys, directories-first, descending size, hidden entries, and a `DirectoryFilter` match.
- [ ] Run `cargo test --locked -p musheen-ui directory::index::tests::external_order_supports_random_ranges -- --exact`; expect failure because no order file exists.
- [ ] Expose the current `views::sort` and `views::group` comparators to the index module as `pub(crate)`. Sort chunks of at most 4,096 `(StoreItem, arrival, record_offset)` values, write sorted offset runs, then merge at most 32 runs per pass. Compare items with the same `ViewPreferences` policy as the in-memory view; use arrival as the final tie-breaker. Build a new fixed-width offset file for each preference/filter generation and publish it only after a complete merge. No `Vec` may grow with total directory size.
- [ ] Build a second offset file sorted by stable `ItemId`. Implement `lookup_id(&ItemId) -> Result<Option<StoreItem>, IndexError>` with binary search over that file. Implement `read_range(Range<usize>) -> Result<Vec<StoreItem>, IndexError>` by seeking the ordered offset file and then the record file. Reject out-of-range, short, and corrupt reads. Count filtered rows while building the order; do not filter only the resident window.
- [ ] Run the focused test and a measured synthetic million-item test. Assert 4,096-or-fewer sort records in memory and at most 32 open run files. Commit `feat(ui): sort directory records on disk`.

## Task 3: Spill and page through the full provider result

- [ ] Replace `streaming_directory_model_pages_through_one_hundred_thousand_items` in `tests/shell.rs` with a failing million-item test. Its provider returns 512 entries per page and a continuation until exactly 1,000,000; assert the final count is 1,000,000, `begin_page()` ends only then, and `items().len() <= 4_096` after every page.
- [ ] Run `cargo test --locked -p musheen-ui --test shell streaming_directory_model_pages_through_one_million_items -- --exact`; expect failure at the old cap or absent indexed count.
- [ ] Remove `DIRECTORY_BROWSER_RETENTION = 100_000` in `app.rs`. Keep one provider page in flight and no more than two pending index pages. Before a page would exceed 4,096 resident models, transfer the current models and that page to a background index operation; only then advance the provider cursor. Add `DirectoryModel::indexed_count()` and `DirectoryModel::visible_count()` so counts do not depend on `view.items().len()`. Generation and cancellation checks must guard page writes and completions.
- [ ] Run the focused test plus `cargo test --locked -p musheen-ui --test shell`. Commit `feat(ui): spill large directories instead of truncating them`.

## Task 4: Virtualized indexed rows

- [ ] Add a failing `app.rs` test using a synthetic indexed tab. Jump to the middle, end, and start. Assert `uniform_list` receives the full visible count, returns placeholders while a range is pending, and then renders the requested items without synchronous file access or more than three viewport heights of components.
- [ ] Run `cargo test --locked -p musheen-ui --lib indexed_directory_rows_render_only_requested_ranges`; expect the old 4,096-row count.
- [ ] Replace `DirectoryRows.positions: Arc<Vec<usize>>` with a source enum: in-memory positions or `{ generation, visible_count }`. In the indexed case, its range callback schedules a background `read_range` and adjacent prefetch when uncached, returns placeholders, and ignores results from older generations. Keep at most 4,096 `StoreItem` values across the viewport cache, selected, focused, and edited items. Reuse the existing small-folder render path.
- [ ] Update the status bar to show indexed partial/complete totals. Run the focused UI test and `cargo test --locked -p musheen-ui --test visual`; commit `feat(ui): render indexed directory ranges`.

## Task 5: Selection, filters, columns, and watch changes

- [ ] Add failing tests in `tests/{views,search,navigation}.rs` for an off-screen selected ID, shift range, select-all, filter, hidden toggle, sort/group change, column parent, and create/remove/rename/metadata watch events after spill. Assert no selected model is pinned beyond the 4,096 total cap, actions resolve the intended lossless path, and scroll anchors survive order swaps.
- [ ] Run the named tests with `cargo test --locked -p musheen-ui --test views`; require the off-screen cases to fail first.
- [ ] Replace full `visible_items()`/`take_visible_items()` paths in indexed interactions with position/ID queries against the worker. Make `DirectoryFilter::matches` available to the index worker. Use a sparse ID set for ordinary selection and a compact arrival-ordinal bitmap for range or select-all; new arrivals after select-all remain unselected. Selection must not create a million `StoreItem` values. Resolve off-screen metadata with `lookup_id` before a command dispatch. Make column parents refer to an index generation and viewport range, not a cloned directory vector.
- [ ] Apply watch events as generation-scoped append records: create/change/rename supersedes an older record with the same ID; removal adds a tombstone. Rebuild a replacement ID/order pair with last-event-wins identity rules, then swap only after both files are complete. Cancel prior work on invalidation, navigation, or tab close. Run the focused tests and `cargo test --locked -p musheen-ui`; commit `feat(ui): keep indexed interactions stable`.

## Task 6: Failure, resource, and release verification

- [ ] Add failing tests for disk full/permission denial, corrupt records, stale page/range completions, cancellation, non-UTF-8 paths, and temporary-file cleanup. Each error must leave the last valid order when safe and never label partial data complete.
- [ ] Run focused tests to observe each expected failure, implement the smallest recovery or error path, and rerun them green. Update `docs/spec/browse.md` with the indexed back-scroll and count mechanism and remove the older plan's selected-model exception.
- [ ] Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, and `cargo test --workspace --locked` with the one-job environment above. Record actual exit codes; do not claim an all-features host gate if native SMB libraries are absent.
- [ ] Run exactly one local Docker package build:

  ```bash
  musheen_ci_scratch=$(mktemp -d -p /home/shawn/workspace2/scratchpads musheen-arch-ci-XXXXXX)
  env DOCKER_CONFIG=/home/shawn/.config/docker-hub/config MUSHEEN_SCRATCH_BASE="$musheen_ci_scratch" scripts/build-arch-package.sh
  ```

  Do not use GitHub CI for development. Review the diff, resource counters, and `BEST_PRACTICES.md` rule 13; commit `test(ui): prove million-item directory browsing`.
