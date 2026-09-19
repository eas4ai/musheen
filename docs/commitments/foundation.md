Status: Draft
Requirements:
  - DEP-001
  - DEP-003
  - DEP-007
  - DEP-008
  - DEP-014
  - DEP-015

# Foundation commitment

## Outcome

The foundation's agreed dependency choices are recorded and can be checked
before implementation begins. No application behavior or code change is in
this commitment while the behavioral requirements remain draft.

## Included requirements

- DEP-001, DEP-003, DEP-007, DEP-008, DEP-014, and DEP-015

The other foundation candidates remain draft specification work and are
not part of this commitment until the developer agrees to them.

## Exit evidence

- `Cargo.toml` and the lockfile use the selected dependency families without
  competing crates for the same roles.
- Source checks find no direct appearance, mount-table, or raw-libc bypass of
  the selected abstraction boundaries.
- The committed dependency set passes license and advisory review.

## Excluded

The store abstraction, browser shell, mutations, remote providers, archives,
desktop services, privileged actions, and terminal remain outside this
dependency-only commitment until their requirements are agreed.
