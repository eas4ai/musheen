Status: Agreed 2026-09-18

# Glossary

Terms defined in this project's sense. This file wins over prior
usage when they conflict.

- **Directory** — a folder on disk. Rejected synonym: "folder"
  (Files wording); the codebase and UI say directory.
- **Pane** — one of up to two side-by-side directory views in a
  window.
- **Tab** — a navigation session (location, history, view state)
  in a window's tab strip. A tab belongs to exactly one pane at a time;
  each pane shows one tab, and a window owns one shared tab strip.
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

## Draft additions

Status: Draft

- **Store path** — a provider-owned, lossless item identifier used for
  operations. A local store path preserves non-UTF-8 Linux path bytes.
- **Display path** — a safe human-readable rendering of a store path. It
  is never accepted as an operation target unless the provider converts
  it back without loss.
- **Stable item identity** — a provider-scoped identity used to reconcile
  selection and metadata across refreshes; it is not a displayed path.
- **Interrupted operation** — durable work that was running when the app
  stopped. It never resumes until the user chooses resume or retry.
- **Portal client** — Musheen asking a desktop portal to select files for
  Musheen. **Portal backend** — Musheen serving file-selection requests
  from other applications.
- **Terminal drawer** — one window-owned terminal session shown in a
  resizable panel below that window's browser content.
- **Supported width** — 720 logical pixels or wider. Widths from 720
  through 959 are narrow; widths of 960 or more are wide.
