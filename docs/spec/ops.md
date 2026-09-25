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

[OPS-013] The engine exposes undo only for an operation whose inverse remains safe and available on the active store.
Falsifier: the UI offers undo for an irreversible or no-longer-valid inverse.
Mechanism: inverse-capability tests after rename, move, trash, overwrite, and remote mutations.
Status: Draft

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

## Creation

[OPS-018] The create action supports empty files and directories through one queued operation after validating the proposed name against the destination store.
Falsifier: creation bypasses the queue or submits a name rejected by the destination.
Mechanism: creation tests for files, directories, invalid names, conflicts, and read-only destinations.
Status: Draft

## Integrity and restart recovery

[OPS-019] Non-atomic writes use a sibling staging name that is never shown as the destination; publication uses the strongest atomic replacement the store declares, and failure leaves the previous destination intact.
Falsifier: readers observe a partially written final item or replacement failure destroys the old destination.
Mechanism: reader-race and injected-publish-failure tests per provider capability.
Status: Draft

[OPS-020] On local durable stores, completion is reported only after file data and the containing directory have been synchronized for operations that claim crash durability.
Falsifier: a crash-durable operation reports completion before its sync calls finish.
Mechanism: syscall-spy ordering tests and crash harness on a disposable filesystem.
Status: Draft

[OPS-021] Copy and move preserve timestamps, mode, ownership, extended attributes, ACLs, and sparse layout when both stores declare support; unsupported metadata is summarized before destructive source removal.
Falsifier: supported metadata changes silently or unsupported metadata is lost during a move without warning.
Mechanism: metadata matrix fixtures across local and limited providers.
Status: Draft

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

[OPS-028] The Trash surface lists trashed items with original location and deletion time, restores selected items through conflict handling, and empties trash only after confirmation naming the item count.
Falsifier: restore overwrites without conflict handling or empty-trash runs without confirmation.
Mechanism: trash list, restore, conflict, and purge integration tests.
Status: Draft

[OPS-029] Archive browse and extraction enforce configurable limits for entry count, expanded bytes, compression ratio, nesting depth, path length, memory, and temporary-disk use before and during decoding.
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
