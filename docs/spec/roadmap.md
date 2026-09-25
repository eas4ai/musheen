
# Roadmap

The roadmap orders implementation so later UI and operations build on
proved storage and command boundaries. A commitment advances only when its
listed requirements have executable checks and those checks pass.

Current: defects-2026-09-25

## 1. Foundation

Establish the Rust workspace, lossless store paths, local provider,
capability matrix, paged models, command registry, native-theme bridge,
settings schema, Lucide icon registry, and Files-derived shell skeleton. This commitment proves
that non-UTF-8 paths and large directories can reach every later layer
without lossy conversion or eager rendering.

## 2. Browse and inspect

Deliver windows, tabs, panes, navigation, layouts, hidden items, search,
preview, Properties dialogs, accessibility, localization, and session
restore over read-only local data.

## 3. Safe local operations

Deliver queued copy, move, create, rename, links, trash, permanent delete,
conflicts, staging, durability, cancellation, journaling, restart recovery,
and the status center on local filesystems.

## 4. Commands and customization

Deliver complete context menus, Settings, toolbar and shortcut editing,
custom actions, tags, pins, home, themes, and per-directory preferences.

## 5. Linux desktop integration

Deliver MIME and application association, FileManager1, portal client and
optional backend, UDisks2 volume actions, notifications, secret storage,
privileged actions, open-in-terminal, and the embedded terminal drawer.

## 6. Archives and remote stores

Deliver bounded archive browse/create/extract and the FTP, FTPS, SFTP,
WebDAV, HTTP, SMB, and mounted-NFS providers through the same capabilities,
operation safety, and recovery contracts.

## 7. Release hardening

Complete performance budgets, fault injection, visual and accessibility
baselines, packaging, update verification, license audit, migration tests,
and cross-desktop integration testing.

## defects-2026-09-25

Requirements: UXF-012, BROWSE-023, OPS-006, OPS-008

Fix the four user-facing defects from docs/opus-audit-2.md section 4:
the sidebar "Open in new tab" crash and "Open" launching a directory
(UXF-012), rubber-band selection that ignores scroll (BROWSE-023), the
cross-device move that leaves a hard-linked source half-removed (OPS-006),
and the trash path that copies across devices without verification
(OPS-008). Done when each mechanism's test fails on the recorded violating
example and passes on the fix, the workspace tests and clippy are clean, and
the review and report are accepted.
