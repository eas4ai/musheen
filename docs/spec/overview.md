Status: Draft

# Musheen — overview

Musheen is a Linux-native file manager that brings the Files app
experience to Linux, rebuilt in Rust on GPUI Kit. It exists because
no current Linux file manager meets the developer's bar and the
Files UX does.

Non-goals: not a line-for-line C# port, no Windows-only surfaces
(WSL, Windows Libraries, Recycle Bin specifics, AppX packaging,
COM server, WinRT previews), no AFP (SMB is the substitute).

## Components

- App shell (tabs, panes, sidebar, toolbar, status bar) owns window
  layout and navigation state.
- Views (list, details, grid, columns, adaptive) render directory
  content from the model.
- Ops engine (copy, move, delete via trash, rename, conflicts,
  progress) owns mutation with undo where the platform allows.
- Core (storage abstraction, filesystem capabilities, watching,
  traversal, paths) owns contact with filesystems and storage providers.
- Desktop glue (MIME, launch, trash, portals, notifications, D-Bus,
  secrets, terminals, and mounts) owns Linux desktop and process contact.
- Shell, views, the model, and the ops policy layer depend only on the
  portable traits exposed by core and desktop glue. Provider and desktop
  implementations may contain platform-specific Rust behind those traits.
- Theme system owns appearance, following the OS through
  native-theme and allowing full user theming in GPUI Kit.

Shell, views, and the ops engine communicate through the model;
core exposes capabilities the UI adapts to instead of failing
mid-operation.

## Technology choices

- Rust + GPUI Kit 0.6.2: production-proven in the developer's
  investment app; semver ranges with a committed lockfile, no exact
  pins.
- Dependency set in docs/spec/deps.md: ecosystem crates per
  capability, custom code only where no crate exists (mimeapps
  resolver, thumbnail cache layer, capability matrix).
- GPL-3.0-or-later, matching the dependency set.

## Spec map

- docs/spec/features.md — validated feature catalog (veracity and
  effort per feature; non-normative, feeds the specs below).
- docs/spec/deps.md (DEP) — decided dependencies.
- docs/spec/core.md (CORE) — storage, capabilities, watching,
  traversal, with core subsystem breakdown.
- docs/spec/browse.md (BROWSE) — tabs, panes, sidebar, toolbar,
  views, sort, group.
- docs/spec/ops.md (OPS) — mutations, conflicts, progress,
  archives, batch rename.
- docs/spec/search.md (SEARCH) — search, filtering, preview,
  properties.
- docs/spec/custom.md (CUSTOM) — settings, themes, actions and
  shortcuts, tags, context-menu policy, and the settings window.
- docs/spec/system.md (SYS) — mounts, trash, MIME and launch,
  D-Bus, portals, terminal, remote.
- docs/spec/ux.md (UXF) — flows, keyboard model, feedback and
  recovery.
- docs/spec/ui.md (UIV) — visual language, states, theming hooks.
- docs/spec/icons.md (ICON) — icon family, semantic mapping, native
  content-icon boundary, and asset rules.
- docs/spec/limits.md (LIMIT) — default resource and timeout budgets.
- docs/spec/roadmap.md — ordered delivery commitments and their exit
  criteria.
