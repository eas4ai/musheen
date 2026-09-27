# Musheen Implementation Plans

These plans turn the contracts in `docs/spec/` into seven independently
reviewable delivery phases. Execute them in order; later plans assume the
public interfaces and checks from earlier plans are committed.

## Entry gate

The specifications are still marked `Draft`. Before implementation begins,
the developer must approve the requirements assigned to the selected phase,
their statuses must be changed to `Agreed`, and the phase commitment must name
only those agreed requirements. Cairn remains disabled until the developer
explicitly restores it.

## Architecture

The root package owns process startup. Workspace crates enforce the dependency
direction below:

```text
musheen (binary)
  -> musheen-ui      -> musheen-ops  -> musheen-core
                     -> musheen-desktop -> musheen-core
  -> musheen-desktop -> musheen-ops
  -> musheen-local   -> musheen-core
  -> musheen-test-support (dev dependencies only)
```

- `musheen-core`: lossless paths, item models, capabilities, store traits,
  paging, cancellation, commands, and resource snapshots.
- `musheen-local`: local filesystem provider, traversal, watch, and metadata.
- `musheen-ops`: mutation scheduler, staging, journal, recovery, and conflicts.
- `musheen-desktop`: Linux MIME, launch, D-Bus, portals, volumes, secrets,
  terminals, archive codecs, and remote adapters. It may submit specialized
  work to `musheen-ops`; `musheen-ops` never imports desktop adapters.
- `musheen-ui`: GPUI Kit shell, views, menus, dialogs, settings, and icon use.
- `musheen-test-support`: recording providers, hostile fixtures, fake desktop
  services, deterministic clocks, and fault injection.

Only `musheen-local` and `musheen-desktop` access browsed filesystem paths.
`musheen-ui` has one narrow exception for its owner-only temporary directory
index in `src/directory/index.rs`; it never opens a browsed path. UI commands
still consult provider capabilities before crossing the operation or desktop
boundary.

## Plan order

1. [Foundation](2026-09-19-01-foundation.md)
2. [Browse and inspect](2026-09-19-02-browse-and-inspect.md)
3. [Safe local operations](2026-09-19-03-safe-local-operations.md)
4. [Commands and customization](2026-09-19-04-commands-and-customization.md)
5. [Linux desktop integration](2026-09-19-05-linux-desktop-integration.md)
6. [Archives and remote stores](2026-09-19-06-archives-and-remote-stores.md)
7. [Release hardening](2026-09-19-07-release-hardening.md)

The [disk-backed directory index](2026-09-24-disk-backed-directory-index.md)
extends the browse phase with million-item pagination and bounded UI memory.

## Requirement ownership

Each plan has a `Primary requirements` line. Every requirement appears in one
primary phase; later phases may add broader evidence for an earlier contract.
Before closing a phase, compare that line with the requirement IDs in the
named spec files and fail the review if an ID is missing or duplicated.
