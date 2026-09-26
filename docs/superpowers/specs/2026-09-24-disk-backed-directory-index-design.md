# Disk-backed directory index design

## Goal and scope

Musheen must browse, sort, and scroll a directory with at least one million
entries without stopping at 100,000 or keeping more than 4,096 `StoreItem`
models in RAM. This implements BROWSE-015, BROWSE-017, BROWSE-019,
BROWSE-020, and LIMIT-002. Search's separate 100,000-result refinement limit
does not change. Small folders keep the current in-memory path.

## Per-tab index

Each tab owns one index generation for its current directory. Once the
in-memory model reaches 4,096 entries, a background worker moves that
generation into a private temporary directory. A versioned, length-bounded
record file stores each item's stable ID, lossless provider path, display metadata, and
arrival ordinal. A fixed-width offset file maps sorted positions to records.
The worker accepts provider pages through a bounded queue and never makes the
GPUI render thread write, sort, or read files. The active provider cursor and
partial or complete count remain attached to the same generation.

The worker sorts bounded runs of at most 4,096 records with the existing
natural-name, group, sort-direction, and directories-first comparators. A
bounded fan-in merge writes a replacement offset file. The arrival ordinal
breaks comparator ties, preserving stable order. A sort, group, hidden-file,
or filter change builds a new order without re-enumerating the provider.
While a replacement builds, the last valid order remains visible with an
updating state; a completed replacement is published atomically. In-view
filtering and hidden-file visibility affect the indexed row count, not only
the resident window.

## Viewport and interaction

`uniform_list` uses the index's visible row count. Its requested range drives
an asynchronous range read and adjacent-range prefetch. Until the range
arrives, it renders placeholders; it never blocks a frame on disk I/O.
Backward scrolling seeks through the sorted offset file rather than restarting
provider enumeration. The resident window, including selected, focused, and
edited items, never exceeds 4,096 `StoreItem` models. Selection remains keyed
by stable item IDs, so evicting a model does not deselect it. Actions that need
metadata resolve IDs through the index before dispatch. Status totals remain
marked partial until the provider reports completion.

## Changes, failures, and lifetime

Watch events append a generation-scoped change record. The worker merges those
changes into a replacement order; unaffected selection and the scroll anchor
remain stable. An invalidation starts a new provider load without accepting
late pages or range results from the prior generation. Navigation, tab close,
and app shutdown cancel pending work and remove the tab's temporary files.
Temporary directories use owner-only permissions; records contain no
credentials. A full disk, corrupt record, failed merge, or unavailable temp
directory produces an actionable directory error and never presents a partial
index as complete. The last valid order remains available where safe.

## Verification

Use a deterministic million-item provider and assert: no 100,000-item stop;
global sort and group order; front, middle, end, and backward range reads;
stable selection and scroll through page arrivals, resort, and watch events;
correct partial and complete totals; at most 4,096 resident item models and
three rendered viewport heights; no provider re-enumeration for back-scroll;
lossless non-UTF-8 paths; stale-generation rejection; and cleanup after
cancellation, navigation, and injected disk errors. Run targeted UI and core
tests, then the locked workspace tests and one local Linux package build with
at most eight Cargo jobs and `CARGO_INCREMENTAL=0`.
