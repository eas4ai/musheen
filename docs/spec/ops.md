Prefix: OPS

# File operations

The ops engine owns mutations, conflict handling, progress, cancellation,
and recovery. It sends work through the store abstraction and reports its
state to the status center.

Review (2026-09-19): checked destructive paths, partial failure, archive
boundaries, and capability refusal. The design keeps presentation out of
the engine and makes unsafe archive paths and irreversible deletion
directly testable.

## Engine and progress

[OPS-001] The ops engine represents copy, move, trash, permanent delete, create, rename, link, compress, and extract work as queued operations.
Falsifier: any listed mutation bypasses the operation queue.
Mechanism: call-boundary check plus queue tests for every operation kind.
Status: Draft

[OPS-002] Each queued operation reports pending, running, paused, completed, failed, cancelled, or interrupted state with item and byte progress when totals are known.
Falsifier: an operation changes state without a corresponding status-center update.
Mechanism: state-transition tests with a recorded progress sink.
Status: Draft

[OPS-003] Cancellation stops scheduling new items, removes unpublished staging data, and never presents partial content as the final destination.
Falsifier: cancellation leaves an incomplete destination under its final name.
Mechanism: fault-controlled tests that cancel during file data, metadata, directory, and cross-store copy phases.
Status: Draft

[OPS-004] The engine records item-level failures while continuing independent items unless the user selected stop-on-error.
Falsifier: one item failure silently discards later independent work.
Mechanism: batch fixture with an unreadable middle item in both error modes.
Status: Draft

## Copy, move, and links

[OPS-005] Copy and cut place a standards-compatible file URI list and the intended operation on the desktop clipboard.
Falsifier: another conforming Linux application cannot read the copied item list.
Mechanism: clipboard integration test against the freedesktop file-transfer format.
Status: Draft

[OPS-006] Move uses an atomic rename when the provider guarantees it. Otherwise it copies to staging, verifies final size and content digest against bytes read, publishes the destination, and deletes the source only after verification.
Falsifier: a cross-store move deletes its source before the verified destination is published.
Mechanism: ops-006
Rationale: 1.x mechanism: same-store and cross-store tests with corruption and failure injected at every phase.
Status: Agreed 2026-09-25

[OPS-007] Link creation offers symbolic links, hard links, and desktop launchers only where the destination capability and source type support them. Hard links are limited to regular files in the same provider and filesystem.
Falsifier: link creation starts where the capability matrix forbids the selected link kind.
Mechanism: capability fixture tests for local same-filesystem, cross-filesystem, FAT-like, directory, and remote destinations.
Status: Draft

## Delete and rename

[OPS-008] Normal delete moves items through the `trash` wrapper when the active location reports trash support. Otherwise it refuses and offers the separate permanent-delete command without choosing it automatically.
Falsifier: normal delete permanently removes an item or silently changes commands on a no-trash provider.
Mechanism: ops-008
Rationale: 1.x mechanism: trash-capable and no-trash provider interaction tests.
Status: Agreed 2026-09-25

[OPS-009] Permanent delete uses the dedicated irreversible-delete boundary and requires confirmation naming the item count, location, and lack of recovery.
Falsifier: a direct removal bypasses that boundary or runs without the confirmation.
Mechanism: call-boundary check and destructive-action tests for every entry point.
Status: Draft

[OPS-010] Inline rename validates the proposed name against the destination store before submitting one rename operation.
Falsifier: an invalid name reaches the store or a valid edit submits twice.
Mechanism: interaction test over valid, empty, reserved, conflicting, and unchanged names.
Status: Draft

[OPS-011] Batch rename previews every resulting name and blocks submission while results collide with each other or with unselected siblings.
Falsifier: the batch engine starts with a collision visible in its preview.
Mechanism: preview tests for templates, counters, replacements, case-only changes, and collisions.
Status: Draft

## Conflicts and recovery

[OPS-012] File conflicts offer keep-both, replace, and skip. Directory conflicts offer merge, replace-tree, keep-both, and skip; replace-tree visibly means the old destination tree is removed before publication. Each choice may be applied to compatible remaining conflicts only.
Falsifier: a conflict performs behavior other than its displayed choice or applies a file-only choice to a directory.
Mechanism: conflict matrix tests over files, directories, symlinks, and mixed batches.
Status: Draft

[OPS-013] The engine exposes undo only for an operation whose inverse remains safe and available on the active store. A trash undo is remembered from the job's own receipt, and its availability is checked against that one trash record and the original location, never by listing the trash.
Falsifier: the UI offers undo for an irreversible or no-longer-valid inverse, or remembering or checking one trash undo lists the trash.
Mechanism: ops-013
Rationale: 1.x mechanism: inverse-capability tests after rename, move, trash, overwrite, and remote mutations; docs/opus-audit-2.md B-M5: each finished trash job and each undo check listed the whole trash, up to a hundred times a second while the status center was open.
Status: Agreed 2026-09-25

## Archives

[OPS-014] Archive creation supports ZIP, tar with gzip or zstd, and 7z through the dependencies assigned in `deps.md`.
Falsifier: a supported format uses a hand-written codec or cannot round-trip fixture content.
Mechanism: dependency check plus round-trip tests for every supported format.
Status: Draft

[OPS-015] Archive browsing exposes entries through a read-only store without extracting the full archive.
Falsifier: opening an archive writes its complete contents to a temporary directory.
Mechanism: integration test that records filesystem writes while browsing a large fixture.
Status: Draft

[OPS-016] Extraction rejects absolute paths, parent traversal, and links that escape the selected destination after platform path normalization.
Falsifier: a crafted archive writes any byte outside the selected destination.
Mechanism: malicious-archive tests covering absolute, `..`, symlink, and hard-link escapes.
Status: Draft

[OPS-017] Encrypted ZIP and 7z operations request credentials through a secret-safe prompt and never persist or log the credential.
Falsifier: an archive credential appears in settings, logs, errors, or operation history.
Mechanism: encrypted round-trip test plus captured-log and persistence scan.
Status: Draft

[OPS-033] Extract… asks for a destination folder, and Extract Here uses the archive's own folder. Either one extracts the archive into a folder named after it inside that folder, under the same limits and path checks. When that folder does not exist, the extraction publishes it in one step. When it exists, the archive's entries merge into it: folders merge entry by entry, and for each entry that collides with an existing file, or with an existing item of the other kind, the user chooses Replace, Replace All, Skip, or Skip All, and the extraction does what was chosen. Nothing else in the folder changes.
Falsifier: choosing a writable local folder for Extract… queues no extraction or writes entries anywhere but the folder named after the archive inside it; Extract Here fails where that folder is absent; a colliding item is replaced without Replace or Replace All, or written after Skip or Skip All; or an item no archive entry collides with changes.
Mechanism: ops-033
Rationale: docs/opus-audit-2.md 4.5 and escalation 407ae943: Extract… was refused after its destination picker, the extract engine refused any existing destination so Extract Here always failed, and the developer asked for a folder named after the archive with a Replace, Replace All, Skip, or Skip All question for each colliding item.
Status: Agreed 2026-09-26

[OPS-034] The extract operation reads the archive where it is and never copies it. It lists and checks every entry in one pass over the archive, then decodes each file once, in archive order, into its staging folder, so it reads the archive's bytes at most twice. It writes nothing but the files it extracts and its own records, and it publishes nothing when the archive changed during the run. When the destination folder exists, the collision check before the questions lists the entries in one pass and writes nothing. An archive inside the archive is extracted as a file and never opened, so no nesting limit applies to it. Extraction has no time limit: the entry, expanded-size, compression-ratio, path, memory, and temporary-space limits bound it, and Cancel stops it.
Falsifier: beyond 1 MiB for the journal and file system records, the extract operation reads more than twice the archive's bytes or writes more than its files' bytes, or the collision check reads more than the archive's bytes or writes anything; either one opens an archive inside the archive; the operation publishes after the archive changed during the run; or it stops because of the time it took.
Mechanism: ops-034
Rationale: docs/opus-audit-2.md C-N3: extraction copied the archive beside the destination, hashed it four times, decoded every file twice to look for nested archives, decoded tar and 7z entries from the start of the archive for each entry, and stopped after 30 seconds of decoding, a limit no requirement names; the developer agreed this text on 2026-09-26.
Status: Agreed 2026-09-26

## Creation

[OPS-018] The create action supports empty files and directories through one queued operation after validating the proposed name against the destination store.
Falsifier: creation bypasses the queue or submits a name rejected by the destination.
Mechanism: creation tests for files, directories, invalid names, conflicts, and read-only destinations.
Status: Draft

## Integrity and restart recovery

[OPS-019] Non-atomic writes use a sibling staging name that is never shown as the destination; publication uses the strongest atomic replacement the store declares, and failure leaves the previous destination intact.
Falsifier: readers observe a partially written final item or replacement failure destroys the old destination.
Mechanism: ops-019
Rationale: docs/opus-audit-2.md B-M1: on a store without RENAME_NOREPLACE the fallback builds the destination in place, and a mid-way error leaves a partial tree under the user-visible name.
Status: Agreed 2026-09-25

[OPS-020] On local durable stores, completion is reported only after file data and the containing directory have been synchronized for operations that claim crash durability.
Falsifier: a crash-durable operation reports completion before its sync calls finish.
Mechanism: syscall-spy ordering tests and crash harness on a disposable filesystem.
Status: Draft

[OPS-021] Copy and move preserve timestamps, mode, ownership, extended attributes, ACLs, and sparse layout when both stores declare support; unsupported metadata is summarized before destructive source removal.
Falsifier: supported metadata changes silently or unsupported metadata is lost during a move without warning.
Mechanism: ops-021
Rationale: docs/opus-audit-2.md B-M2: sparse files inside a copied or moved folder are fully expanded with no warning while single files keep their holes.
Status: Agreed 2026-09-25

[OPS-022] Operations treat a symlink as the selected item by default and follow its target only after an explicit follow-links choice; traversal still rejects loops.
Falsifier: copying or deleting a symlink mutates its target without that choice.
Mechanism: symlink-to-file, symlink-to-directory, broken-link, and loop fixtures.
Status: Draft

[OPS-023] Same-store copy preserves hard-link relationships and reflinks when the provider supports them; otherwise it copies bytes and reports the fallback without claiming link preservation.
Falsifier: a supported hard-link set is expanded into unrelated files or a fallback result is reported as linked.
Mechanism: inode, reflink, and fallback fixture tests.
Status: Draft

[OPS-024] Operations whose read/write sets overlap are serialized or rejected before execution, including parent-child moves and concurrent renames.
Falsifier: two accepted operations can race to create an order-dependent or self-containing result.
Mechanism: scheduler tests over overlapping source and destination graphs.
Status: Draft

[OPS-025] Preflight checks estimated bytes, item count, destination capacity, quota, write access, and operation-specific provider limits; estimates remain labeled and runtime exhaustion preserves published data.
Falsifier: a known-insufficient destination starts without warning or an out-of-space failure corrupts an existing item.
Mechanism: quota, capacity, and mid-write exhaustion fixtures.
Status: Draft

[OPS-026] The operation journal is schema-versioned and atomically persisted before execution and at state transitions. On restart, previously running work is marked interrupted and requires resume, retry, or discard.
Falsifier: restarted work is displayed as running or mutates storage before the user chooses a recovery action.
Mechanism: process-kill tests at every persisted transition plus migration fixtures.
Status: Draft

[OPS-027] Resume continues only a provider operation with a verified continuation point; retry starts an idempotent replacement operation, and discard removes only app-owned staging data after showing what will remain.
Falsifier: recovery repeats a completed mutation or deletes user-owned data.
Mechanism: restart recovery tests for copy, move, archive, and remote fixtures.
Status: Draft

[OPS-028] The Trash surface lists trashed items with original location and deletion time, restores selected items through conflict handling, and empties trash only after confirmation naming the item count. One unreadable trash entry is listed as unrestorable and purgeable; it never hides the others.
Falsifier: restore overwrites without conflict handling, a restorable item fails to restore or leaves a stray entry at its original path, one entry whose payload is missing makes the listing fail, or empty-trash runs without confirmation.
Mechanism: ops-028
Rationale: docs/opus-audit-2.md B-M3 and B-M4: restoring a trashed link to a directory failed and left an empty directory, and one orphaned .trashinfo made the whole Trash view fail.
Status: Agreed 2026-09-25

[OPS-029] Archive browse and extraction enforce configurable limits for entry count, expanded bytes, compression ratio, path length, memory, and temporary-disk use before and during decoding; browsing also limits nesting depth.
Falsifier: a crafted archive can exceed any configured budget without a bounded error and cleanup.
Mechanism: archive-bomb fixtures for each independent limit.
Status: Draft

[OPS-030] At execution time, destructive operations revalidate stable identity and operate relative to an already opened parent without following the selected item's final symlink component.
Falsifier: replacing a checked path with a symlink or different item can redirect deletion or overwrite to another target.
Mechanism: rename-race, symlink-swap, and path-reuse fault tests.
Status: Draft

[OPS-031] Regular files, directories, and symlinks have explicit copy behavior; devices, sockets, and FIFOs are refused unless a future provider declares a separate safe operation. They are never opened and copied as ordinary bytes.
Falsifier: copying a special file reads from or writes to its device or stream.
Mechanism: local fixtures for block, character, socket, FIFO, and broken-link items.
Status: Draft

[OPS-032] Recursive copy, delete, size, search, and permission work does not cross a nested mount or provider boundary unless the user explicitly includes that boundary in the displayed scope.
Falsifier: a recursive operation silently mutates a mounted filesystem below its selected root.
Mechanism: nested-mount and nested-provider scope tests for every recursive command.
Status: Draft
