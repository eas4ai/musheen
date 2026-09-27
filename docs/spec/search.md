Prefix: SEARCH

# Search, preview, and properties

This subsystem turns the current browser context into filtered or searched
results and describes selected items without mutating them. Property edits
are submitted to the ops engine.

Review (2026-09-19): checked empty and multi-selection states, stale
asynchronous results, thumbnail cache rules, permission failures, and
secret exposure. Search and preview cancellation are observable in tests.

## Search and filtering

[SEARCH-001] Search mode recursively queries the active pane's location and displays the searched scope beside the query.
Falsifier: identical visible scope text searches a different location or pane.
Mechanism: dual-pane search test with distinct nested fixtures and matching names.
Status: Draft

[SEARCH-002] The result model streams matches without blocking navigation and discards results from a superseded query.
Falsifier: a slow earlier query adds results after a later query becomes active.
Mechanism: asynchronous ordering test with delayed mock providers.
Status: Draft

[SEARCH-003] In-view filtering narrows the loaded directory model without changing the tab location or navigation history.
Falsifier: clearing a filter requires navigation to restore previously loaded items.
Mechanism: model test that filters, navigates history assertions, and clears.
Status: Draft

[SEARCH-004] Search and filtering expose name, type, modified-time, and size criteria only when the active store can supply the required metadata.
Falsifier: the UI accepts a criterion that the active store cannot evaluate.
Mechanism: criterion-availability tests over complete and limited provider fixtures.
Status: Draft

## Info pane and preview

[SEARCH-005] The info pane shows explicit empty, single-selection, multi-selection, loading, preview, details, and error states.
Falsifier: any selection or load state reuses another state without an identifying cue.
Mechanism: component-state tests for every listed state.
Status: Draft

[SEARCH-006] Text and code preview reads a bounded prefix by default and requires an explicit action before loading content beyond that bound. Binary or undecodable content is labeled instead of lossy-decoded as text.
Falsifier: selecting an arbitrarily large text file loads the whole file into memory.
Mechanism: memory-bounded preview test with a sparse large-file fixture.
Status: Draft

[SEARCH-007] Preview work is cancelled when its selection, tab, or window is no longer active.
Falsifier: obsolete preview work updates the pane or continues holding its file handle.
Mechanism: cancellation test with a delayed preview provider and handle accounting.
Status: Draft

## Thumbnails

[SEARCH-008] The thumbnail pipeline decodes untrusted content in restartable worker processes outside the UI process and enforces bounds for concurrent work, source bytes, decoded pixels, memory, and wall time.
Falsifier: a thumbnail blocks UI input or exceeds any configured decode budget.
Mechanism: responsiveness, worker-crash, and limit tests over huge, malformed, and deliberately stalled images.
Status: Draft

[SEARCH-009] The thumbnail cache follows the freedesktop path, naming, size, source mtime, and failure-record rules.
Falsifier: a stale thumbnail or active failure record triggers a normal cache hit.
Mechanism: cache fixture tests for size classes, changed mtimes, misses, and fail records.
Status: Draft

[SEARCH-010] Cache-only thumbnail requests never decode source content.
Falsifier: a cache-only miss opens the source through a decoder.
Mechanism: provider-spy test for cache hit, cache miss, and stale entry.
Status: Draft

## Properties dialogs

[SEARCH-011] The file Properties dialog reports identity, type, MIME, location, size, timestamps, ownership, permissions, hashes, default application, tags, and applicable filesystem capabilities in General, Permissions, Open With, Tags, and Checksums pages.
Falsifier: an available listed field is omitted or a missing field is shown as known.
Mechanism: property-model tests over local, read-only, and metadata-limited fixtures.
Status: Draft

[SEARCH-012] The directory Properties dialog adds contained item count, allocated size, mount/filesystem data, sharing state when available, and an explicitly started recursive size calculation that is cancellable.
Falsifier: opening directory properties starts unbounded recursive work or omits an available listed field.
Mechanism: dialog tests over small, huge, mount-root, and remote directories.
Status: Draft

[SEARCH-013] Multi-selection Properties labels aggregate values and never presents one item's mutable value as shared unless all selected items match.
Falsifier: a mixed permission, owner, or type value appears as a common value.
Mechanism: mixed-selection property tests.
Status: Draft

[SEARCH-014] Permission and ownership edits show their planned recursive scope before the user submits them to the ops engine.
Falsifier: a recursive metadata change starts without displaying its affected scope.
Mechanism: properties interaction test for single item, directory-only, and recursive edits.
Status: Draft

[SEARCH-015] The Checksums page computes user-selected BLAKE3 and SHA256 values as cancellable streaming background work and labels a result with its algorithm, stable identity, size, and mtime. A change during reading invalidates the result.
Falsifier: a changed file retains a hash presented as current.
Mechanism: hash test that mutates or replaces a fixture during and after computation.
Status: Draft

[SEARCH-016] Recursive search uses provider paging, a bounded result channel, and cancellable backpressure; the UI virtualizes results and never retains an unbounded rendered row set.
Falsifier: a slow consumer causes unbounded memory growth or navigation waits for search completion.
Mechanism: delayed million-result provider test with memory and cancellation assertions.
Status: Draft

[SEARCH-017] Search follows the active tab's hidden-item policy and does not follow symlinks unless the user enables follow-links for that query; the scope summary displays both choices.
Falsifier: the same displayed scope silently changes hidden or symlink behavior.
Mechanism: query fixtures containing hidden entries, symlinks, and loops.
Status: Draft

[SEARCH-018] Unreadable subdirectories and disconnected provider pages produce partial results with itemized scope errors and retry, rather than discarding matches.
Falsifier: one permission or network error replaces valid prior results.
Mechanism: recursive search tests with mid-tree permission and network failures.
Status: Draft

[SEARCH-019] The Permissions page of a local item's Properties shows its access as Dolphin does and changes it only after Apply. Owner, Group and Others each choose No Access, Can View or Can View & Modify for files, and No Access, Can View Content or Can View & Modify Content for folders, where viewing a folder's content includes entering it. A file has an Allow executing file as program checkbox: checking it adds execute for each class that may read the file, and clearing it removes execute for all three. A selection whose items differ, or mode bits no choice describes, shows Varies and stays as it is unless the user picks a choice. The owner is chosen by name from the system's user accounts, and the group by name from the system's groups. Unless Musheen runs as the superuser, the user changes modes only on items they own and may set only one of their own groups, only on items they own; every other owner or group change needs administrator rights, the page says so before Apply, and Apply makes it as administrator, as SYS-037 says. Advanced Permissions shows the read, write and execute bits of the three classes and the setuid, setgid and sticky bits, which can be changed there, and lists the ACL entries, read-only, with user and group names. When named ACL entries exist, the Group row sets their mask, and the page says so. Apply to contents extends a folder's change to what it contains after the user reviews that scope, with the file choices for files and the folder choices for folders. Nothing changes before Apply, which runs through the operations queue and changes modes before owners and groups; an item the change leaves as it is does not fail it, and No Access on a file the user owns can be undone. On a filesystem without POSIX permissions the page is read-only and says why. Every string on the page is localized.
Falsifier: a choice, the checkbox or an Advanced bit sets a mode other than the one described; a mixed selection shows one item's value as shared, or Apply changes something the user did not change; a mode change reaches an item whose mode the user may not change, or an owner or group the user may not set alone changes without administrator authorization or without the page saying first that it needs it; the page changes anything before Apply or outside the operations queue; Apply fails because a selected item needs no change; No Access on the user's own file cannot be undone; the page offers edits on a filesystem without POSIX permissions; or a string on the page is not localized.
Mechanism: search-019
Rationale: Shawn's rulings of 2026-09-26 and 2026-09-27: a Dolphin-style page in place of numeric IDs and octal modes; owner and group changes as administrator follow SYS-037 (item admin-ownership-changes); ACL editing comes later (item acl-editing).
Status: Agreed 2026-09-27
