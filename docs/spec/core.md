Status: Draft
Prefix: CORE

# Core

Portable foundation. Everything above core programs against the
store abstraction and the capability matrix; only core touches the
OS. Subsystems: paths, traversal, watch, capabilities, store.

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
The implementation passes every filesystem path through the paths
boundary helpers and never feeds std::path strings directly into
filesystem calls.
Falsifier: a filesystem call site takes Path or PathBuf without
boundary conversion.
Mechanism: grep check for std::fs use outside the paths and store
modules.

## Traversal

[CORE-002]
Status: Draft
The implementation enumerates directories through walkdir with
bounded file descriptors and never follows symlinks unless the
caller opts in.
Falsifier: enumeration hangs or exhausts descriptors on a symlink
loop fixture.
Mechanism: symlink-loop fixture test with descriptor accounting.

## Watch

[CORE-003]
Status: Draft
The implementation watches directories with notify and marks any
location whose filesystem has no notify backend as polled, so the
UI never claims live updates it does not have.
Falsifier: a location on a backend-less filesystem displays live
status.
Mechanism: backend-matrix review plus an integration test with a
stubbed unsupported backend.

## Capabilities

[CORE-004]
Status: Draft
Core exposes a capability matrix per mount covering permissions,
symlinks, extended attributes, reflink copies, trash support, and
case sensitivity.
Falsifier: a mount lacks a matrix entry for any of the six
capabilities.
Mechanism: fixture test over mocked statfs types including ext4,
btrfs, FAT variants, and tmpfs.

[CORE-005]
Status: Draft
Capability entries derive from filesystem type plus runtime probes
and never from hardcoded paths.
Falsifier: a capability branch matches a literal path prefix.
Mechanism: grep check for hardcoded mount paths in core.

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
The local provider refuses operations the capability matrix
forbids for that location and names the reason instead of failing
mid-operation.
Falsifier: a forbidden operation starts and then errors instead
of being refused up front.
Mechanism: fixture tests attempting forbidden operations per
filesystem type.
