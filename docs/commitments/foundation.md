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
before implementation begins. It includes only dependency-enablement changes,
such as the recorded native-theme connector compatibility patch. It adds no
user-facing application behavior while the behavioral requirements remain draft.

## Included requirements

- DEP-001, DEP-003, DEP-007, DEP-008, DEP-014, and DEP-015

The other foundation candidates remain draft specification work and are
not part of this commitment until the developer agrees to them.

## Exit evidence

- `Cargo.toml` selects the agreed app-facing dependency families without a
  second direct provider for the same role, and the lockfile resolves their
  approved versions. Internal crates used only by an approved dependency are
  transitive implementation details, not additional Musheen role providers.
- Source checks find no direct appearance, mount-table, or raw-libc bypass of
  the selected abstraction boundaries.
- The committed dependency set passes license review.

## Excluded

The store abstraction, browser shell, mutations, remote providers, archives,
desktop services, privileged actions, and terminal remain outside this
dependency-only commitment until their requirements are agreed.
