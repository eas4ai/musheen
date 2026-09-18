Status: Draft

# Validated feature catalog

Non-normative. Each entry records veracity (does Files really do
this, with the citing path), effort to build on Linux in Rust
(S/M/L/XL), and disposition (port, adapt, cut, replace, new).
Normative requirements live in the subsystem specs named per entry.
Entries marked Unresolved need evidence before they harden.

## Browse (subsystem: browse.md)

- Tabs: multi-tab, Ctrl+1..9 switching, duplicate, tear-out,
  session restore, reopen-closed. Validated
  (Files.App ViewModels/MainPageViewModel.cs, Views TabBar). Effort
  M. Disposition: port.
- Dual panes with open-in-other and focus control. Validated
  (ShellPanesPage.xaml.cs). Effort M. Disposition: port.
- Sidebar: home, pinned, drives, cloud, network, tags sections,
  per-tab expansion, drag-drop. Validated
  (SidebarViewModel.cs). Effort M. Disposition: adapt (Linux
  sections: places, mounts, network, tags).
- Toolbar: back/forward history, up, refresh, omnibar
  path/search/command modes, breadcrumbs, search suggestions.
  Validated (NavigationToolbar.xaml.cs,
  NavigationToolbarViewModel.cs). Effort M. Disposition: port.
- Views: details, list, cards, grid, columns, adaptive; per-folder
  prefs; per-layout icon sizes. Validated (LayoutAction.cs,
  FolderLayoutModes.cs, LayoutPreferencesManager.cs). Effort L.
  Disposition: port.
- Sort, group, header-click toggle, folders-together. Validated
  (SortAction.cs, GroupAction.cs, GroupOption.cs). Effort S.
  Disposition: port.
- Status bar with counts. Validated (StatusBar.xaml). Effort S.
  Disposition: port.

## File operations (subsystem: ops.md)

- Copy/move via clipboard model plus direct-move contract.
  Validated (CopyItemAction.cs, CutItemAction.cs,
  IDirectMove.cs). Effort M. Disposition: adapt (Linux clipboard
  and GIO semantics).
- Delete with recycle-vs-permanent routing and confirmation.
  Validated (BaseDeleteAction.cs, DeleteItemAction.cs). Effort S.
  Disposition: replace (trash crate, freedesktop spec).
- Rename: inline single plus bulk-rename dialog (F2). Validated
  (RenameAction.cs, BulkRenameDialogViewModel). Effort M.
  Disposition: port, dialog rebuilt in Kit.
- Conflict options: generate-new-name, replace, skip. Validated
  (FileNameConflictResolveOptionType.cs). Effort S. Disposition:
  port (portable concept, WinUI shell cut).
- Operation progress model with status center. Validated
  (StatusCenterItemProgressModel.cs,
  FilesystemOperationDialog.xaml.cs). Effort M. Disposition: port,
  dialog rebuilt in Kit.
- Bulk engine: queued copy/move/delete/new/rename with progress
  sink. Validated (WindowsBulkOperations.cs). Effort L.
  Disposition: adapt (custom engine on Rust fs primitives).
- Archive create (.zip) plus browse-and-extract. Validated
  (CompressIntoZipAction.cs, ZipStorageFolder.cs). Effort M.
  Disposition: replace (zip, tar, sevenz-rust2 crates).

## Search, preview, properties (subsystem: search.md)

- Omnibar search entry plus in-view filtering. Validated
  (NavigationToolbar, BaseShellPage filter). Effort M.
  Disposition: port.
- Info pane with preview and details states. Validated (InfoPane
  dispatch). Effort M. Disposition: port.
- Code/text preview. Validated. Effort S. Disposition: port.
- Properties dialog with portable core
  (MainPropertiesViewModel). Validated. Effort M. Disposition:
  adapt (Linux property pages: permissions, ownership).
- Hashes view. Validated (HashesViewModel). Effort S.
  Disposition: port.
- Shell/D3D previews, WinRT media/image/PDF, AQS search backend,
  ADS pages, Security/Compatibility pages. Validated. Effort n/a.
  Disposition: cut, replaced by Linux rows below.

## Settings, actions, tags (subsystem: custom.md)

- Settings pages: general, appearance, layout, folders.
  Validated (SettingsPageViewModel). Effort M. Disposition: adapt
  (Linux settings, no registry).
- Action/command system: IAction, CommandManager, customizable
  toolbar. Validated. Effort M. Disposition: port.
- Tags with settings service and home widget. Validated
  (TagsViewModel, FileTagsSettingsService, FileTagsWidget).
  Effort M. Disposition: port.
- Pinned/favorites and home. Validated (HomeViewModel). Effort S.
  Disposition: port.
- ElementTheme/Mica appearance, registry-backed advanced and
  recent-files, COM QuickAccess, classic properties dialog.
  Validated. Effort n/a. Disposition: cut.

## System integration (subsystem: system.md)

- Launcher and full-trust server processes. Validated
  (FilesLauncher.cpp, Server Program.cs). Effort n/a.
  Disposition: cut as designed; Linux equivalents specified in
  system.md (FileManager1, portals).
- Open/Save dialogs as COM components. Validated. Effort n/a.
  Disposition: replace (ashpd FileChooser).
- Background update task (jump-list refresh, log cleanup).
  Validated (UpdateTask.cs). Effort S. Disposition: adapt (no
  jump lists on Linux; log rotation and update checks remain).
- Terminal integration. No Files evidence (Windows Terminal
  handoff out of scope in tree). Effort M. Disposition: new
  (Linux terminal launch plus embedded panel per Dolphin row).

## Core (subsystem: core.md)

- Storage abstraction with provider backends. Validated
  (Files.Core.Storage, Files.App.Storage). Effort L.
  Disposition: adapt (Linux capability matrix per DEP-007).
- Filesystem capability differences by type (fat16, fat32, ext4,
  btrfs and others). No Files evidence (Windows-only
  filesystems). Effort M. Disposition: new, specified in core.md.

## Linux gaps from reference managers (subsystems as noted)

- Unix permissions and ownership editing. Validated against
  Dolphin and Nautilus sources. Effort M. Disposition: new
  (core.md, search.md properties).
- Mount and volume handling with eject. Validated against
  Dolphin, Nautilus, Thunar-era sources. Effort M. Disposition:
  new (system.md).
- MIME, default apps, Open With, icons, launching. Validated
  against reference sources plus user Layers 1-5. Effort L
  (custom mimeapps resolver is the long pole). Disposition: new
  (system.md).
- FileManager1 service, portals, notifications. Validated
  against Nautilus and Dolphin sources. Effort M. Disposition:
  new (system.md).
- Batch rename with templates and renamers. Validated against
  Nautilus and Dolphin sources. Effort M. Disposition: new
  (ops.md).
- Remote protocols (FTP/SFTP/WebDAV/SMB/NFS). Validated against
  Dolphin and Nautilus sources. Effort XL, staged last.
  Disposition: new (system.md).
- Thumbnail pipeline with spec cache. Validated against Nautilus
  sources. Effort M. Disposition: new (search.md or core.md at
  decomposition time).

## Resolved drawers (reviewed 2026-09-18, all bodies opened)

- Server class set: Program.cs registers sealed public
  Files.App.Server classes as WinRT activation factories
  (CsWinRT); the only such class is AppInstanceMonitor, which
  exits the helper when client PIDs die. Open/Save dialogs are
  separate COM servers with fixed CLSIDs. Cut, no portable code;
  the lifecycle pattern (helper exits with its clients) is
  already the FileManager1 design.
- StorageSecurityService: Windows ACLs via GetNamedSecurityInfo
  and SID strings with a PowerShell-as-admin fallback. Cut;
  the get/set-owner concept is already the Linux permissions row.
- WindowsCompatibilityService: get/set Windows compat-mode shims
  per executable. Cut, nothing portable.
- Shortcut write (PasteItemAsShortcutAction into
  UIFilesystemHelpers.PasteItemAsShortcutAsync): creates .lnk
  files. Cut .lnk; the paste-as-link concept adapts to symlinks
  and .desktop links (ops.md).
- FileThumbnailHelper: WinRT/font/MTP/Win32 icon fetching on an
  STA thread. Cut; three ideas stolen for the thumbnail design:
  DPI-scaled request sizes, background-thread fetching (our
  rayon pool), and cache-only fetch mode (matches fail/cache
  records).
- SettingsSearchIndexer: walks settings pages to build a
  settings-search index (page, group, card). Portable concept,
  no Windows dependency. Port into custom.md and ux.md.
