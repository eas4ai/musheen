Status: Agreed 2026-09-18

# Glossary

Terms defined in this project's sense. This file wins over prior
usage when they conflict.

- **Directory** — a folder on disk. Rejected synonym: "folder"
  (Files wording); the codebase and UI say directory.
- **Pane** — one of up to two side-by-side directory views in a
  window.
- **Tab** — a navigation session (location, history, view state)
  in the tab bar; each pane shows one tab.
- **Omnibar** — the toolbar field that switches between path
  entry, search, and command modes.
- **Info pane** — the side panel showing preview and details for
  the selection.
- **Status center** — where background and long operations report
  progress.
- **Capability matrix** — core's per-filesystem table of what the
  underlying filesystem can do (permissions, symlinks, reflink,
  trash).
- **Mount** — a filesystem attached at a path, shown in the
  sidebar with eject where supported.
- **MIME chain** — detection (xdg-mime) to default resolution
  (custom mimeapps) to parsing and icons (freedesktop) to launch.
- **Theme bridge** — native-theme plus its GPUI connector mapping
  the OS theme onto Kit's theme, before user theming applies.
- **Custom action** — a user-defined context entry (Thunar-UCA
  style), distinct from built-in actions.
- **Batch rename** — renaming many files at once from templates
  and renamers, with conflict detection.
- **Settings search** — finding a setting by typing, built from
  an index walked off the settings pages.
