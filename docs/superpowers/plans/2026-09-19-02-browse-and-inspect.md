# Browse and Inspect Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver complete read-only navigation, directory views, search, previews, and file/folder Properties over local data.

**Architecture:** Extend the foundation model with independent tab/pane navigation state and bounded background pipelines. Views consume paged models; search, preview, thumbnail, and metadata workers communicate through cancellation-aware channels and never block GPUI rendering.

**Tech Stack:** Foundation stack plus xdg-mime, bounded tree_magic_mini fallback, image, fast_image_resize, posix-acl, xattr, blake3, and sha2.

---

**Primary requirements:** BROWSE-002–003, BROWSE-005, BROWSE-007–011,
BROWSE-014, BROWSE-016, BROWSE-020–022; SEARCH-001–013, SEARCH-015–018; DEP-002,
DEP-005, DEP-010, DEP-019–020; ICON-009–011; LIMIT-003–005; UXF-002,
UXF-004, UXF-022; UIV-011–013, UIV-021.

### Task 1: Make navigation state independent and restorable

**Files:** Create `crates/musheen-ui/src/navigation/{mod,history,tab,pane,session,omnibar,breadcrumbs}.rs`;
create `crates/musheen-ui/tests/navigation.rs`.

- [ ] Test independent back/forward stacks, duplicate tabs, split panes,
  per-pane selection, path/search/command omnibar modes, tab-relative path
  suggestions, breadcrumb overflow, and restoration after one stored location
  disappears.
- [ ] Run `cargo test -p musheen-ui --test navigation`; expect failure.
- [ ] Implement serializable `WindowSession`, `TabState`, and `PaneState` with
  provider-owned paths; persist only after debouncing and atomic replacement.
- [ ] Bind all navigation surfaces to registry commands and keep focus in the
  content view after successful keyboard navigation.
- [ ] Run the test and manually exercise two windows with two panes each.
- [ ] Commit with `feat(ui): add tabs panes and durable navigation state`.

### Task 2: Complete sidebar and directory presentations

**Files:** Create `crates/musheen-ui/src/views/{mod,list,details,grid,columns,adaptive,sort,group,selection}.rs`;
modify `sidebar.rs` and `directory.rs`; create `crates/musheen-ui/tests/views.rs`.

- [ ] Write model tests for list/details/grid/columns/adaptive switching,
  natural sort, folders-first policy, grouping, hidden-file visibility,
  rubber-band selection, rename retention, and scroll anchoring across pages.
- [ ] Run `cargo test -p musheen-ui --test views`; expect failure.
- [ ] Implement a shared virtualized item model so presentation changes do not
  reload the store. Keep selected and edited items pinned outside the normal
  4,096-model retention cap.
- [ ] Render sidebar sections for Home, pins, devices, cloud/remote locations,
  tags, and network; hide empty sections instead of showing false providers.
  Keep expansion state per tab while shared pin changes update every tab.
- [ ] Inject external create/remove/rename/metadata events and verify stable
  selection, scroll anchor, sort, grouping, and column layout survive.
- [ ] Run the million-item fixture and assert rendered-node/model counters.
- [ ] Commit with `feat(ui): add complete directory views and sidebar`.

### Task 3: Add bounded search and filtering

**Files:** Create `crates/musheen-core/src/search.rs`,
`crates/musheen-local/src/search.rs`, and
`crates/musheen-ui/src/search/{mod,query,results,filters}.rs`; create
`crates/musheen-local/tests/search.rs` and `crates/musheen-ui/tests/search.rs`.

- [ ] Test name/content queries, scope, glob, MIME, size, date, hidden policy,
  invalid expressions, cancellation, a stalled consumer, and the 100,000-result
  Refine Search boundary.
- [ ] Run both search tests; expect missing-search failures.
- [ ] Implement parsed `SearchQuery` and streaming `SearchBatch` values. Use
  256-result batches, a 2,048-result channel, and 4,096 off-screen models.
- [ ] Keep provider-specific search behind the `Store` boundary and expose
  explicit partial-result/error states.
- [ ] Run the fast million-result producer test under the memory counter.
- [ ] Commit with `feat(search): add cancellable bounded local search`.

### Task 4: Build MIME, preview, and thumbnail pipelines

**Files:** Create `crates/musheen-desktop/src/{mime,preview,thumbnail}.rs`,
`crates/musheen-ui/src/info_pane.rs`; create tests in
`crates/musheen-desktop/tests/{mime,preview,thumbnail}.rs`.

- [ ] Test extension and content MIME detection, generic-result fallback,
  non-UTF-8 names, stale thumbnail mtime, fail records, oversized image headers,
  malformed decodes, 10-second timeout, and four-worker concurrency.
- [ ] Run the three tests; expect failure.
- [ ] Implement xdg-mime as primary and tree_magic_mini only for bounded unknown
  results. Isolate thumbnail decoding in a worker process and write the
  freedesktop cache atomically with source mtime metadata.
- [ ] Implement text preview reads of 1 MiB initially, explicit 16 MiB chunks,
  and a 64 MiB ceiling while always preserving Open With.
- [ ] Run fixture tests with sparse 1 TiB files and decompression-heavy images.
- [ ] Commit with `feat(desktop): add bounded preview and thumbnail workers`.

### Task 5: Implement Properties and metadata models

**Files:** Create `crates/musheen-desktop/src/{properties,checksum,permissions}.rs`;
create `crates/musheen-ui/src/dialogs/{mod,properties,permissions,open_with}.rs`;
create `crates/musheen-ui/tests/properties.rs`.

- [ ] Test single file, folder, symlink, multi-selection, mixed ownership,
  recursive-size cancellation, xattrs, ACL display, capability-disabled edits,
  BLAKE3/SHA-256 known answers, and replacement of a selected item mid-dialog.
- [ ] Run `cargo test -p musheen-ui --test properties`; expect failure.
- [ ] Implement a snapshot-plus-live-update properties model. Stream checksums
  and recursive sizes without whole-file buffering. Render permission and ACL
  values read-only in this phase; Phase 3 adds validated mutation plans.
- [ ] Use the semantic dialog pattern for title, content, validation summary,
  default action, cancel action, and focus restoration.
- [ ] Run the test plus keyboard-only and screen-reader tree checks.
- [ ] Commit with `feat(ui): add file and folder properties dialogs`.

### Task 6: Finish accessibility, localization, and visual evidence

**Files:** Create `crates/musheen-ui/src/i18n.rs`, `locales/en-US.ftl`,
`locales/en-XA.ftl`, and `crates/musheen-ui/tests/{accessibility,visual}.rs`.

- [ ] Add tests that every interactive node has a role/name/state, view changes
  preserve focus, icons have semantic fallbacks, and pseudo-localized strings
  do not clip at narrow width or 200% scale.
- [ ] Register `assets/icons/musheen.svg` for application/package identity and
  verify content icons continue to come from the active freedesktop theme.
- [ ] Capture deterministic baselines for every view, search, preview,
  Properties variant, empty/error/loading state, and theme mode.
- [ ] Run the complete workspace checks, `cargo deny check`, and release build.
- [ ] Perform the rule 13 self-review and commit with
  `test: close browse and inspect acceptance evidence`.
