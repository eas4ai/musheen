Status: Draft
Prefix: SEARCH

# Search, preview, and properties

This subsystem turns the current browser context into filtered or searched
results and describes selected items without mutating them. Property edits
are submitted to the ops engine.

Review (2026-09-19): checked empty and multi-selection states, stale
asynchronous results, thumbnail cache rules, permission failures, and
secret exposure. Search and preview cancellation are observable in tests.

## Search and filtering

[SEARCH-001]
Status: Draft
Search mode recursively queries the active pane's location and displays
the searched scope beside the query.
Falsifier: identical visible scope text searches a different location or pane.
Mechanism: dual-pane search test with distinct nested fixtures and matching names.

[SEARCH-002]
Status: Draft
The result model streams matches without blocking navigation and discards
results from a superseded query.
Falsifier: a slow earlier query adds results after a later query becomes active.
Mechanism: asynchronous ordering test with delayed mock providers.

[SEARCH-003]
Status: Draft
In-view filtering narrows the loaded directory model without changing the
tab location or navigation history.
Falsifier: clearing a filter requires navigation to restore previously loaded items.
Mechanism: model test that filters, navigates history assertions, and clears.

[SEARCH-004]
Status: Draft
Search and filtering expose name, type, modified-time, and size criteria
only when the active store can supply the required metadata.
Falsifier: the UI accepts a criterion that the active store cannot evaluate.
Mechanism: criterion-availability tests over complete and limited provider fixtures.

## Info pane and preview

[SEARCH-005]
Status: Draft
The info pane shows explicit empty, single-selection, multi-selection,
loading, preview, details, and error states.
Falsifier: any selection or load state reuses another state without an identifying cue.
Mechanism: component-state tests for every listed state.

[SEARCH-006]
Status: Draft
Text and code preview reads a bounded prefix by default and requires an
explicit action before loading content beyond that bound. Binary or
undecodable content is labeled instead of lossy-decoded as text.
Falsifier: selecting an arbitrarily large text file loads the whole file into memory.
Mechanism: memory-bounded preview test with a sparse large-file fixture.

[SEARCH-007]
Status: Draft
Preview work is cancelled when its selection, tab, or window is no longer
active.
Falsifier: obsolete preview work updates the pane or continues holding its file handle.
Mechanism: cancellation test with a delayed preview provider and handle accounting.

## Thumbnails

[SEARCH-008]
Status: Draft
The thumbnail pipeline decodes untrusted content in restartable worker
processes outside the UI process and enforces bounds for concurrent work,
source bytes, decoded pixels, memory, and wall time.
Falsifier: a thumbnail blocks UI input or exceeds any configured decode budget.
Mechanism: responsiveness, worker-crash, and limit tests over huge,
malformed, and deliberately stalled images.

[SEARCH-009]
Status: Draft
The thumbnail cache follows the freedesktop path, naming, size, source
mtime, and failure-record rules.
Falsifier: a stale thumbnail or active failure record triggers a normal cache hit.
Mechanism: cache fixture tests for size classes, changed mtimes, misses, and fail records.

[SEARCH-010]
Status: Draft
Cache-only thumbnail requests never decode source content.
Falsifier: a cache-only miss opens the source through a decoder.
Mechanism: provider-spy test for cache hit, cache miss, and stale entry.

## Properties dialogs

[SEARCH-011]
Status: Draft
The file Properties dialog reports identity, type, MIME, location, size,
timestamps, ownership, permissions, hashes, default application, tags, and
applicable filesystem capabilities in General, Permissions, Open With,
Tags, and Checksums pages.
Falsifier: an available listed field is omitted or a missing field is shown as known.
Mechanism: property-model tests over local, read-only, and metadata-limited fixtures.

[SEARCH-012]
Status: Draft
The directory Properties dialog adds contained item count, allocated size,
mount/filesystem data, sharing state when available, and an explicitly
started recursive size calculation that is cancellable.
Falsifier: opening directory properties starts unbounded recursive work or
omits an available listed field.
Mechanism: dialog tests over small, huge, mount-root, and remote directories.

[SEARCH-013]
Status: Draft
Multi-selection Properties labels aggregate values and never presents one
item's mutable value as shared unless all selected items match.
Falsifier: a mixed permission, owner, or type value appears as a common value.
Mechanism: mixed-selection property tests.

[SEARCH-014]
Status: Draft
Permission and ownership edits show their planned recursive scope before
the user submits them to the ops engine.
Falsifier: a recursive metadata change starts without displaying its affected scope.
Mechanism: properties interaction test for single item, directory-only, and recursive edits.

[SEARCH-015]
Status: Draft
The Checksums page computes user-selected BLAKE3 and SHA-256 values as
cancellable streaming background work and labels a result with its
algorithm, stable identity, size, and mtime. A change during reading
invalidates the result.
Falsifier: a changed file retains a hash presented as current.
Mechanism: hash test that mutates or replaces a fixture during and after computation.

[SEARCH-016]
Status: Draft
Recursive search uses provider paging, a bounded result channel, and
cancellable backpressure; the UI virtualizes results and never retains an
unbounded rendered row set.
Falsifier: a slow consumer causes unbounded memory growth or navigation waits
for search completion.
Mechanism: delayed million-result provider test with memory and cancellation assertions.

[SEARCH-017]
Status: Draft
Search follows the active tab's hidden-item policy and does not follow
symlinks unless the user enables follow-links for that query; the scope
summary displays both choices.
Falsifier: the same displayed scope silently changes hidden or symlink behavior.
Mechanism: query fixtures containing hidden entries, symlinks, and loops.

[SEARCH-018]
Status: Draft
Unreadable subdirectories and disconnected provider pages produce partial
results with itemized scope errors and retry, rather than discarding matches.
Falsifier: one permission or network error replaces valid prior results.
Mechanism: recursive search tests with mid-tree permission and network failures.

[SEARCH-019]
Status: Draft
The Permissions page exposes POSIX owner, group, user/group/other mode,
executable state, and ACL entries only when supported. Recursive changes
separate file and directory modes, show privilege needs, and enter the ops
queue only after Apply.
Falsifier: a control claims unsupported metadata, combines file and directory
recursive modes invisibly, or mutates before Apply.
Mechanism: Properties tests over owned, foreign-owned, ACL, read-only, and remote items.
