
# Roadmap

The roadmap orders implementation so later UI and operations build on
proved storage and command boundaries. A commitment advances only when its
listed requirements have executable checks and those checks pass.

Current: ui-thread-growth-2026-09-25

## 1. Foundation

Establish the Rust workspace, lossless store paths, local provider,
capability matrix, paged models, command registry, native-theme bridge,
settings schema, Lucide icon registry, and Files-derived shell skeleton. This commitment proves
that non-UTF-8 paths and large directories can reach every later layer
without lossy conversion or eager rendering.

## 2. Browse and inspect

Deliver windows, tabs, panes, navigation, layouts, hidden items, search,
preview, Properties dialogs, accessibility, localization, and session
restore over read-only local data.

## 3. Safe local operations

Deliver queued copy, move, create, rename, links, trash, permanent delete,
conflicts, staging, durability, cancellation, journaling, restart recovery,
and the status center on local filesystems.

## 4. Commands and customization

Deliver complete context menus, Settings, toolbar and shortcut editing,
custom actions, tags, pins, home, themes, and per-directory preferences.

## 5. Linux desktop integration

Deliver MIME and application association, FileManager1, portal client and
optional backend, UDisks2 volume actions, notifications, secret storage,
privileged actions, open-in-terminal, and the embedded terminal drawer.

## 6. Archives and remote stores

Deliver bounded archive browse/create/extract and the FTP, FTPS, SFTP,
WebDAV, HTTP, SMB, and mounted-NFS providers through the same capabilities,
operation safety, and recovery contracts.

## 7. Release hardening

Complete performance budgets, fault injection, visual and accessibility
baselines, packaging, update verification, license audit, migration tests,
and cross-desktop integration testing.

## defects-2026-09-25

Requirements: UXF-012, BROWSE-023, OPS-006, OPS-008

Fix the four user-facing defects from docs/opus-audit-2.md section 4:
the sidebar "Open in new tab" crash and "Open" launching a directory
(UXF-012), rubber-band selection that ignores scroll (BROWSE-023), the
cross-device move that leaves a hard-linked source half-removed (OPS-006),
and the trash path that copies across devices without verification
(OPS-008). Done when each mechanism's test fails on the recorded violating
example and passes on the fix, the workspace tests and clippy are clean, and
the review and report are accepted.

## local-safety-index-2026-09-25

Requirements: OPS-019, OPS-021, OPS-028, BROWSE-019, BROWSE-023

Fix the local data-safety and large-folder findings from docs/opus-audit-2.md
sections 5.1 and 5.2: the no-replace fallback that leaves a partial tree
under the destination name (OPS-019), sparse files expanded silently inside
copied or moved folders (OPS-021), Trash restore of a directory link and a
listing that fails on one orphaned entry (OPS-028), and the directory index
that lives in the temp dir, leaks on SIGTERM, drops the shown items when its
first write fails, collapses the selection on Ctrl-click after Select All,
and leaves the rubber band and the Columns layout inert (BROWSE-019,
BROWSE-023). Done when each mechanism's test fails on the recorded violating
example and passes on the fix, the workspace tests and clippy are clean, and
the review and report are accepted.

## ui-thread-growth-2026-09-25

Requirements: UXF-023, BROWSE-020, UIV-014, OPS-013, LIMIT-010

Fix the UI-thread and growth findings from docs/opus-audit-2.md sections
5.1 and 5.2: catalog file I/O on every watch event and stat and statfs on
every context menu and command target check (UXF-023), the whole-order
rebuild on every watch event in an indexed directory and its silent
failure (BROWSE-020), the fallback theme that always ends on light
(UIV-014), the full trash listing on every finished trash job and undo
check (OPS-013), and the scheduler records, status history and persisted
status document that grow without bound (LIMIT-010). Done when each
mechanism's test fails on the recorded violating example and passes on
the fix, the workspace tests and clippy are clean, and every finding of
the review and the report is resolved or declined.

## catalog-writes-off-ui-thread

Requirements: UXF-023

Take the remaining catalog file I/O and the store calls that come with it
off the UI thread (backlog item catalog-writes-off-ui-thread, escalation
54ca2f0b). The item names Properties tag edits and per-directory view
preferences; the same wait is also in navigation, which resolves the
folder identity and records recents and the remembered location on every
load; in sidebar tag rename and delete, which ask each tagged path's
capabilities; in the catalog projection sync, which resolves every pin
and rewrites the catalog on each catalog change; in Home orphan cleanup;
and in the move and rename completion, which asks statfs and rewrites
the catalog. The resolutions of review 75af775d finding 1 and report
9fa1a7b3 finding 5 said a09935f moved the move completion off the UI
thread; it did not, and this commitment does. The initial catalog read
when a window is built stays where it is. Done when a test for each of
these paths holds the catalog lock from another open file or blocks the
store and the window still repaints and takes input, the paths still
apply their change once the lock or the store is released, the workspace
tests and clippy are clean, and every finding of the review and the
report is resolved or declined.
