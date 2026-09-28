Prefix: BROWSE

# Browse

The app shell owns windows, tabs, panes, navigation, directory views,
and the status bar. It reads directory data through the model and does
not access storage directly.

Review (2026-09-19): checked the Files-derived catalog against the
Linux component boundaries in `overview.md`. The requirements separate
navigation state from filesystem access and give every visible state a
fixture or interaction test.

## Tabs and panes

[BROWSE-001] Each tab belongs to exactly one pane at a time and preserves its location, navigation history, selection, scroll position, layout, sort, group, and hidden-item state while the window remains open.
Falsifier: switching away from a tab and back loses any preserved state.
Mechanism: interaction test that changes every tab state field before two tab switches.
Status: Draft

[BROWSE-002] The window's shared tab strip supports create in the focused pane, close, duplicate, reorder within a pane, move to the other pane, tear-out to a new window, and reopen-closed actions.
Falsifier: any listed tab action is absent or changes the source tab unexpectedly.
Mechanism: window-level interaction tests for each tab action.
Status: Draft

[BROWSE-003] The session store restores open windows, tab order, pane assignment, and active locations after a normal restart.
Falsifier: a normal restart opens a different restorable browsing session.
Mechanism: restart test over a two-window, multi-tab session fixture.
Status: Draft

[BROWSE-004] Each window presents either one pane or two side-by-side panes with one pane designated as focused.
Falsifier: an action targets a pane other than the visibly focused pane.
Mechanism: interaction test that alternates focus before navigation and file actions.
Status: Draft

[BROWSE-005] The browser can open a selected location in the other pane without changing the source pane.
Falsifier: open-in-other-pane changes the source pane or opens the wrong location.
Mechanism: dual-pane interaction test with distinct source and destination histories.
Status: Draft

## Navigation

[BROWSE-006] Each tab provides back, forward, parent, and refresh navigation with disabled states when an action is unavailable.
Falsifier: an unavailable navigation action remains enabled or changes location.
Mechanism: navigation-state tests at a root, a child directory, and both ends of history.
Status: Draft

[BROWSE-007] The omnibar exposes distinct path, search, and command modes and always shows which mode will receive the next submission.
Falsifier: the same visible omnibar state can submit text to two different modes.
Mechanism: interaction tests for mode entry, submission, cancellation, and restoration.
Status: Draft

[BROWSE-008] Path-mode suggestions resolve relative to the current location and never navigate until the user confirms a result.
Falsifier: highlighting a suggestion changes location or resolves from another tab.
Mechanism: suggestion test with equal child names in two tabs.
Status: Draft

[BROWSE-009] Breadcrumbs expose every ancestor that the current store can address and allow navigation to any exposed ancestor.
Falsifier: a reachable ancestor is missing or a breadcrumb resolves to another location.
Mechanism: interaction test over local and mocked remote location hierarchies.
Status: Draft

## Sidebar

[BROWSE-010] The sidebar groups home, pinned locations, mounts, network locations, and tags into distinct sections.
Falsifier: an available item appears in no section or in the wrong section.
Mechanism: sidebar model test with one fixture item of each kind.
Status: Draft

[BROWSE-011] Each tab preserves its own sidebar expansion state while pinned content remains shared across tabs.
Falsifier: expanding a tree in one tab changes another tab's expansion state.
Mechanism: two-tab sidebar state test with shared pin mutations.
Status: Draft

[BROWSE-012] The sidebar accepts file and directory drops only when the target exposes a supported operation through the capability matrix.
Falsifier: an unsupported target displays an accepting drop state or starts an operation.
Mechanism: drag-and-drop tests over writable, read-only, and non-file targets.
Status: Draft

## Directory views

[BROWSE-013] The content area offers details, list, cards, grid, columns, and adaptive layouts over the same directory model. Columns shows folder levels side by side: selecting a folder opens the next column, selecting a sibling replaces its descendants, and the active column uses the same item actions as other layouts. Older levels remain reachable through breadcrumbs and Back.
Falsifier: changing layout changes directory contents, or column navigation loses the parent path or keeps a stale descendant after sibling selection.
Mechanism: model-equivalence and nested/sibling column-navigation tests.
Status: Draft

[BROWSE-014] The preference store remembers layout and icon size by provider and lossless store path, never by display text, and falls back to user defaults for unseen directories.
Falsifier: reopening a directory uses another directory's preference or ignores its own.
Mechanism: persistence test with two directories and changed global defaults.
Status: Draft

[BROWSE-015] The directory model supports sort and group keys with an independent directories-first option.
Falsifier: changing directories-first silently changes the chosen sort or group key.
Mechanism: ordering tests over mixed files, directories, names, sizes, types, and dates.
Status: Draft

[BROWSE-016] The details header toggles the active sort column between ascending and descending order.
Falsifier: repeated activation produces an order other than ascending then descending.
Mechanism: header interaction test for every sortable column.
Status: Draft

[BROWSE-017] The status bar reports total item count, selected item count, and selected byte size when known. During paged loading it labels totals as partial or unknown rather than presenting loaded rows as the final total.
Falsifier: the status bar disagrees with the visible model or labels a partial total as complete.
Mechanism: selection tests over empty, mixed, paged, and size-unknown fixtures.
Status: Draft

[BROWSE-018] Show Hidden toggles dot-prefixed entries for the active tab without changing another tab or the search scope after it has started.
Falsifier: toggling hidden entries changes another tab or loses a selected hidden item without explaining its disappearance.
Mechanism: multi-tab interaction tests with hidden selections and active search.
Status: Draft

[BROWSE-019] Directory views virtualize rendered items and request provider pages ahead of the viewport, while preserving selection and focus by stable item identity. After 4,096 items, each tab uses a private disk index under the user's cache directory for global sorting and back-scrolling, and every layout and every selection command works over that index. At most 4,096 item models remain in memory, including selected, focused, and edited items; when the index cannot be written, the items already shown stay and the error is visible. The status bar marks counts partial until the provider's final page is indexed; then it shows the full count, including folders with at least one million entries. Closing or navigating away from a tab removes its index, termination by SIGTERM or SIGINT removes the live indexes, and indexes left behind by an earlier process are removed at startup.
Falsifier: opening a million-item fixture creates one rendered component per item, loses selection as pages arrive, or cannot scroll back to an evicted row; the index is written outside the cache directory; a failed index write drops the items already shown; Ctrl-click after Select All collapses the selection; the rubber band or the Columns layout does nothing in an indexed folder; an index directory survives its tab, a SIGTERM, or the next start.
Mechanism: browse-019
Rationale: docs/opus-audit-2.md 5.1 (A-F5, A-F6, A-F9, the index kept in a tmpfs temp dir, and index directories left behind on SIGTERM).
Status: Agreed 2026-09-25

[BROWSE-020] External create, remove, rename, and metadata changes reconcile into the open model without resetting unrelated selection, scroll, sort, or grouping. In an indexed directory a change merges into the on-disk order in place, reading only the records it is compared against; the changes that arrive while one merge runs are applied by the next merge as one batch; a merge that fails keeps the shown items and makes the error visible.
Falsifier: one external change reloads the view to its top or selects a different stable item; in an indexed directory, one change decodes more than 100 records of a 100,000-record index, a batch of waiting changes costs one merge each, or a failed merge leaves the list silently stale.
Mechanism: browse-020
Rationale: 1.x mechanism: watcher and polling tests during selection and scrolling; docs/opus-audit-2.md A-F3: every watch event rebuilt the whole order in five to seven passes over all records under the index mutex, and a failed merge was never shown.
Status: Agreed 2026-09-25

[BROWSE-021] The browsing-session store is schema-versioned, writes through atomic replacement with a last-valid backup, and migrates supported older sessions before restoring windows and tabs.
Falsifier: interrupted persistence destroys the last valid session or a supported old session is interpreted without migration.
Mechanism: kill-during-write, corrupt-primary, backup, and version migration tests.
Status: Draft

[BROWSE-022] Details view lets the user show, hide, reorder, and resize available metadata columns and persists that layout by directory with a global default. Columns whose metadata is unavailable remain visibly unknown rather than fabricated.
Falsifier: changing column layout affects another directory with its own preference or missing metadata is displayed as a real value.
Mechanism: column persistence and limited-provider model tests.
Status: Draft

[BROWSE-023] Rubber-band selection selects exactly the items whose rendered rows or cells intersect the dragged rectangle, in every layout and at every scroll position.
Falsifier: after scrolling, a drag rectangle selects rows other than the ones it visibly covers.
Mechanism: browse-023
Rationale: Observed on 2026-09-25 in docs/opus-audit-2.md section 4.2; the developer authorized the audit items ("You audit items are all authorized").
Status: Agreed 2026-09-25

[BROWSE-024] A desktop entry file in a folder shows the entry's name, in the user's language when the entry translates it, and the entry's icon, when the entry is trusted: the user may execute it, or it lies in an XDG applications folder. An untrusted entry shows its file name and the generic desktop-entry icon. The file name always shows in the item's tooltip and in the info pane, Rename changes the file name, and sorting by name uses the name shown. Musheen reads the entry from the file it opened and checked, and an icon the entry names by an absolute path is decoded by the thumbnail worker within its limits.
Falsifier: an untrusted entry shows a name or icon taken from its content; a trusted entry shows its file name instead of its name; the file name is missing from the tooltip or the info pane; Rename changes the entry's name instead of the file name; or an icon named by an absolute path is decoded in the UI process.
Mechanism: browse-024
Rationale: Shawn's ruling of 2026-09-27 (item desktop-entry-display): Dolphin shows an entry's name and icon; only a trusted entry does here, so a downloaded file cannot pose as a document.
Status: Agreed 2026-09-28
