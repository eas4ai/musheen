# Commands and Customization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver coherent menus, Settings, shortcut/toolbar editing, themes, custom actions, tags, pins, Home, and per-folder preferences.

**Architecture:** The command registry remains the sole source of executable actions. Menus, toolbars, shortcuts, and the palette are projections filtered by selection, provider capabilities, policy, and current state. Settings are versioned domain sections with validated transactions and immediate previews.

**Tech Stack:** Existing workspace, GPUI Kit controls, native-theme, XDG config/data paths, wax patterns, and the operation/desktop boundaries from earlier phases.

---

**Primary requirements:** CUSTOM-001, CUSTOM-003, CUSTOM-005–008,
CUSTOM-010–011, CUSTOM-013–021, CUSTOM-023–035; UXF-006, UXF-018–019;
UIV-020, UIV-022.

### Task 1: Turn the command registry into the action authority

**Files:** Modify `crates/musheen-core/src/command.rs`; create
`crates/musheen-core/src/context.rs`; create
`crates/musheen-core/tests/command_registry.rs`.

- [x] Test uniqueness of IDs/shortcuts, deterministic enablement, capability
  refusal reasons, selection cardinality, writable destination rules, checked
  states, dangerous-action classification, and localization/icon completeness.
- [x] Run `cargo test -p musheen-core --test command_registry`; expect failure.
- [x] Add `CommandContext`, `CommandState`, `DangerLevel`, and typed parameters.
  A command handler receives validated IDs/paths, never display strings.
- [x] Register every action specified in CUSTOM-016–028 and prove each appears
  in exactly one registry entry.
- [x] Run the registry audit and snapshot its stable public IDs.
- [x] Commit with `feat(commands): centralize all user actions`.

### Task 2: Compose complete context menus

**Files:** Create `crates/musheen-ui/src/menus/{mod,builder,context,open_with,send_to}.rs`;
create `crates/musheen-ui/tests/context_menus.rs`.

- [ ] Add table-driven tests for empty space, one file, one folder, mixed
  selection, archive, executable, hidden item, read-only provider, trash,
  clipboard state, privileged location, right-click selection preservation,
  and keyboard targeting of the focused item or view background.
- [ ] Assert coverage for Open, Open With and association selection, Send To,
  Cut, Copy, Copy To, Move To, Paste, Rename, Duplicate, soft link, hard link,
  Compress, Extract, Hide, Unhide, Trash, permanent delete, Properties,
  permissions, Run as Administrator, and Open as Administrator.
- [ ] Test Copy To and Move To chooser cancellation, destination preflight,
  provider capability refusal, conflict routing, and command identity.
- [ ] Run `cargo test -p musheen-ui --test context_menus`; expect failure.
- [ ] Implement policy-driven sections, stable ordering, native-theme styling,
  submenu overflow, disabled explanations, and destructive confirmations.
  Unsupported actions are disabled or omitted before invocation.
- [ ] Run keyboard navigation, screen-reader tree, RTL, and pseudo-locale tests.
- [ ] Commit with `feat(ui): add capability-aware context menus`.

### Task 3: Build the Settings window and migrations

**Files:** Create `crates/musheen-desktop/src/settings/{document,migrate,validate}.rs`;
create `crates/musheen-ui/src/settings/{mod,window,general,appearance,files,search,operations,terminal,remote,advanced}.rs`;
create `crates/musheen-ui/tests/settings.rs`.

- [ ] Test category search, validation, cancel/apply semantics, live appearance
  preview with rollback, reset-by-section, corrupt recovery, unknown-field
  preservation, and migration from every earlier schema fixture.
- [ ] Run `cargo test -p musheen-ui --test settings`; expect failure.
- [ ] Implement a single-instance Settings window. Each section edits a draft;
  apply validates the whole transaction and writes atomically. Resource-limit
  controls show defaults, hard maxima, units, and restart requirements.
- [ ] Keep Settings and Properties non-modal. Trap focus only in conflict,
  authorization, and destructive confirmation dialogs, then restore it to the
  exact prior control. Dialog-local keys take precedence over browser keys.
- [ ] Ensure secret fields store only a credential reference; terminal,
  privilege, and remote settings remain hidden until their owning feature is
  available.
- [ ] Run tests plus light/dark/high-contrast and 200% visual baselines.
- [ ] Commit with `feat(settings): add validated settings window`.

### Task 4: Add toolbar and shortcut editors

**Files:** Create `crates/musheen-ui/src/settings/{toolbar,shortcuts}.rs` and
`crates/musheen-core/src/customization.rs`; create
`crates/musheen-ui/tests/customization.rs`.

- [ ] Test add/remove/reorder, duplicate prevention, required navigation escape,
  shortcut conflicts across scopes, reserved OS combinations, import/export,
  reset, and orphaned command IDs after upgrade.
- [ ] Run the customization test; expect failure.
- [ ] Store only command IDs and presentation preferences. Render current label,
  icon, enablement, and handler from the registry so custom surfaces cannot
  fork behavior.
- [ ] Add accessible drag alternatives and immediate preview with cancel rollback.
- [ ] Run the test and manually verify mouse-free editing.
- [ ] Commit with `feat(ui): add toolbar and shortcut customization`.

### Task 5: Add themes and custom actions safely

**Files:** Create `crates/musheen-ui/src/theme/{document,validate,preview}.rs`;
create `crates/musheen-desktop/src/custom_action.rs`; create
`crates/musheen-ui/tests/themes.rs` and
`crates/musheen-desktop/tests/custom_actions.rs`.

- [ ] Test native-follow, explicit light/dark, high contrast, invalid/missing
  tokens, theme import rollback, custom action placeholders, shell injection
  strings, multi-selection, timeout, exit status, and missing executable.
- [ ] Run both tests; expect failure.
- [ ] Implement theme documents as semantic token overrides over GPUI Kit; no
  theme may replace command/content icon families or remove focus indicators.
- [ ] Launch custom actions directly with an argv vector and explicit working
  directory. Shell execution requires an independently labeled opt-in action;
  never interpolate filenames into a shell string.
- [ ] Run security fixtures containing quotes, newlines, leading dashes, and
  non-UTF-8 names.
- [ ] Commit with `feat(custom): add themes and safe custom actions`.

### Task 6: Implement tags, pins, Home, and folder preferences

**Files:** Create `crates/musheen-desktop/src/catalog/{mod,tags,pins,home,folder_prefs}.rs`;
modify sidebar and views; create `crates/musheen-desktop/tests/catalog.rs`.

- [ ] Test stable item identity, missing/moved targets, duplicate pins, tag
  rename/delete, Home aggregation, per-folder view/sort inheritance, removable
  media, inaccessible paths, and catalog recovery after interrupted writes.
- [ ] Run the catalog test; expect failure.
- [ ] Implement a versioned XDG-data catalog keyed by provider/item identity
  with path hints for repair. Never write Musheen metadata into user folders
  unless that provider explicitly supports and the user enables it.
- [ ] Integrate tags and pins into sidebar, search filters, Properties, and
  context menus through registry commands.
- [ ] Run catalog, search, and navigation regression suites.
- [ ] Commit with `feat(catalog): add tags pins and folder preferences`.

### Task 7: Close the phase

- [ ] Generate a command-surface matrix and verify every context/toolbar/menu/
  shortcut entry resolves to one registry command with one capability policy.
- [ ] Run format, Clippy, workspace tests, locked release build, license audit,
  accessibility checks, and deterministic visual baselines.
- [ ] Perform the rule 13 self-review and commit with
  `test: close commands and customization evidence`.
