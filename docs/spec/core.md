Status: Draft
Prefix: CORE

# Core

Portable storage foundation. Everything above core programs against the
store abstraction and capability matrix. Core may use operating-system
filesystem APIs; Linux desktop services remain in desktop glue.
Subsystems: paths, traversal, watch, capabilities, store.

Review (2026-09-18): checked for contradictions (CORE-006 and
CORE-007 overlap deliberately: one bans direct fs access, the
other requires capability honoring; different falsifiers, both
checkable), for unfalsifiable requirements (CORE-005 names a
forbidden pattern, checkable by grep), and for uncheckable
mechanisms (CORE-003 polling fallback is verified by backend
matrix review, the weakest mechanism here, flagged in-file).

## Paths

[CORE-001]
Status: Draft
Every operation uses a provider-owned store path. The local store path
preserves `OsStr`/`OsString` bytes losslessly, including non-UTF-8 names,
and display text is a separate value that cannot be used as an operation
target without a lossless conversion.
Falsifier: a non-UTF-8 local name cannot be listed and renamed, or a
lossy display string is passed to a filesystem call.
Mechanism: hostile-path fixtures plus a type-boundary source check.

## Traversal

[CORE-002]
Status: Draft
The local provider uses `walkdir` only for recursive traversal, bounds
open descriptors, and never follows symlinks unless the caller opts in.
Falsifier: local traversal hangs or exhausts descriptors on a symlink
loop fixture.
Mechanism: local symlink-loop fixture test with descriptor accounting.

## Watch

[CORE-003]
Status: Draft
The local provider watches directories with `notify`; providers without
a reliable watch backend declare polling or manual-refresh semantics so
the UI never claims live updates it does not have.
Falsifier: a location displays live status while its provider supplies no
watch or polling updates.
Mechanism: provider contract tests with notify, polling, and manual-only
fixtures.

## Capabilities

[CORE-004]
Status: Draft
Core exposes a capability matrix per store location covering permissions,
ownership, symlinks, hard links, sparse files, extended attributes,
reflink copies, trash, atomic rename, watching, and case sensitivity.
Falsifier: a location lacks an explicit supported, unsupported, or unknown
entry for any listed capability.
Mechanism: provider fixtures including ext4, btrfs, FAT-like, archive,
HTTP, SMB, and metadata-limited stores.

[CORE-005]
Status: Draft
Local capability entries derive from filesystem type plus runtime probes;
other providers report their own capabilities. No provider infers a
capability from a hardcoded display path.
Falsifier: a capability branch matches a literal display-path prefix.
Mechanism: source check plus provider capability fixtures.

## Store

[CORE-006]
Status: Draft
All directory and file access above core goes through the store
abstraction with the local provider as the first backend.
Falsifier: view or ops code calls std::fs directly.
Mechanism: grep check for std::fs use outside core and the local
provider.

[CORE-007]
Status: Draft
Every provider refuses operations its capability matrix forbids and names
the reason before queueing work.
Falsifier: a known-forbidden operation starts and then errors instead of
being refused up front.
Mechanism: provider contract tests attempting each forbidden operation.

[CORE-008]
Status: Draft
Each provider implements paged directory enumeration with stable item
identity, cancellation, and bounded buffering; remote and archive
providers do not route enumeration through `walkdir`.
Falsifier: a provider must load an entire directory before yielding an
item or loses cancellation between pages.
Mechanism: provider contract test over delayed million-item fixtures.

[CORE-009]
Status: Draft
Watch overflow, event gaps, and provider reconnect invalidate the affected
directory and trigger a bounded rescan that reconciles items by stable
identity.
Falsifier: an overflow leaves the model permanently stale or duplicates
items after rescan.
Mechanism: injected overflow and reconnect tests during external changes.

[CORE-010]
Status: Draft
Clipboard, URI, D-Bus, and portal boundaries either round-trip a store
path without loss or refuse the transfer with the affected item named.
Falsifier: a boundary silently substitutes a different path for an
unrepresentable item.
Mechanism: non-UTF-8 boundary tests for every exported path format.
