Status: Draft
Requirements:
  - CORE-001
  - CORE-002
  - CORE-003
  - CORE-004
  - CORE-005
  - CORE-006
  - CORE-007
  - CORE-008
  - CORE-009
  - CORE-010
  - DEP-001
  - DEP-003
  - DEP-007
  - DEP-008
  - DEP-014
  - DEP-015
  - BROWSE-001
  - BROWSE-004
  - BROWSE-006
  - BROWSE-013
  - BROWSE-015
  - BROWSE-017
  - BROWSE-018
  - BROWSE-019
  - CUSTOM-002
  - CUSTOM-004
  - CUSTOM-009
  - CUSTOM-012
  - UXF-001
  - UXF-003
  - UXF-007
  - UXF-014
  - UXF-015
  - UXF-020
  - UIV-001
  - UIV-002
  - UIV-003
  - UIV-004
  - UIV-005
  - UIV-006
  - UIV-007
  - UIV-008
  - UIV-009
  - UIV-010
  - UIV-014
  - UIV-015
  - UIV-016
  - UIV-017
  - UIV-018
  - UIV-024
  - LIMIT-001
  - LIMIT-002
  - ICON-001
  - ICON-002
  - ICON-003
  - ICON-004
  - ICON-005
  - ICON-006
  - ICON-007
  - ICON-008

# Foundation commitment

## Outcome

A runnable Musheen shell can browse local directories read-only through the
store abstraction, including non-UTF-8 names and paged large directories.
It follows the active native Linux theme and routes every visible action
through one command registry. No file mutation is in this commitment.

## Included requirements

- CORE-001 through CORE-010
- DEP-001, DEP-003, DEP-007, DEP-008, DEP-014, and DEP-015
- BROWSE-001, BROWSE-004, BROWSE-006, BROWSE-013, BROWSE-015,
  BROWSE-017, BROWSE-018, and BROWSE-019
- CUSTOM-002, CUSTOM-004, CUSTOM-009, and CUSTOM-012
- UXF-001, UXF-003, UXF-007, UXF-014, UXF-015, and UXF-020
- UIV-001 through UIV-010, UIV-014 through UIV-018, and UIV-024
- LIMIT-001 and LIMIT-002
- ICON-001 through ICON-008

## Exit evidence

- A fixture directory containing non-UTF-8, symlink-loop, hidden, and
  million-item provider cases passes the core provider contract suite.
- Light, dark, high-contrast, reduced-motion, narrow-width, 200-percent,
  and pseudo-localized shell baselines pass deterministic comparison.
- Accessibility-tree checks find no unnamed or unreachable shell control.
- `cargo fmt --check`, lint, test, locked build, dependency license audit,
  and release build pass on the supported Linux CI image.

## Excluded

Mutations, remote providers, archives, portals, D-Bus services, UDisks2,
privileged actions, and the terminal are later commitments. Their commands
may appear only as disabled development placeholders and cannot ship in a
foundation release.
