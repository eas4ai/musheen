Status: Draft
Prefix: CUSTOM

# Settings, themes, actions, and tags

This subsystem owns durable user preferences, theme overrides, command
registration, toolbar and shortcut customization, tags, pinned locations,
and the home surface.

Review (2026-09-19): checked invalid configuration, command collisions,
missing targets, unavailable filesystem metadata, and native-theme
fallbacks. Each user customization has a reset or recovery path.

## Settings

[CUSTOM-001]
Status: Draft
The Settings window groups settings into General, Appearance, Layout,
Files and Directories, Search and Preview, Operations, Integrations,
Shortcuts, and Advanced pages.
Falsifier: a persisted public setting has no owning page or appears on an unrelated page.
Mechanism: settings-schema test that maps every public key to one page.

[CUSTOM-002]
Status: Draft
The settings store validates loaded values and replaces an invalid value
with its documented default without discarding valid neighboring values.
Falsifier: one malformed value prevents the remaining settings from loading.
Mechanism: corrupted-settings fixtures with one invalid value per supported type.

[CUSTOM-003]
Status: Draft
The Settings window uses one non-modal instance per application, preserves
its current page and search query while open, and never blocks file operations.
Falsifier: a second Settings command creates conflicting windows or an open
Settings window prevents browser work.
Mechanism: multi-window Settings and background-operation interaction tests.

[CUSTOM-004]
Status: Draft
The settings store has an explicit schema version, writes by temporary-file
sync and atomic replacement, preserves the last valid backup, and applies
ordered migrations before exposing values.
Falsifier: interruption can replace valid settings with a partial file or a
supported old schema loads without its migration.
Mechanism: kill-during-write tests and one migration fixture per schema version.

[CUSTOM-005]
Status: Draft
Each Settings page exposes Restore Page Defaults, and the window exposes
Restore All Defaults with a summary and confirmation; both immediately use
the same validation and live-apply rules as manual edits.
Falsifier: reset leaves a page-owned value behind or silently resets values
outside the displayed scope.
Mechanism: schema ownership and reset tests over every public setting.

[CUSTOM-006]
Status: Draft
The settings schema assigns startup, click behavior, session, and recent
history to General; system mode, overrides, density, icons, and motion to
Appearance; default views, panes, sidebar, info pane, terminal drawer, and
toolbar to Layout; hidden items, directories-first, executable handling,
and per-directory reset to Files and Directories; search scope, symlinks,
previews, thumbnails, and cache limits to Search and Preview; conflict,
confirmation, concurrency, and archive limits to Operations; terminal,
portal backend, notifications, mounts, privilege, and credentials to
Integrations; bindings to Shortcuts; and logs, updates, caches, diagnostics,
and custom actions to Advanced.
Falsifier: a listed behavior has no setting on its assigned page or appears
under two owning pages.
Mechanism: settings-schema inventory test against the registered behavior keys.

[CUSTOM-007]
Status: Draft
The app applies settings that can change safely at runtime without a
restart and labels any setting that requires reopening a window.
Falsifier: an unlabeled setting appears saved but does not affect the running app.
Mechanism: live-settings interaction tests plus restart-required schema check.

[CUSTOM-008]
Status: Draft
Settings search indexes page, group, setting label, and supported keyword
aliases from the same schema that renders the settings pages.
Falsifier: a rendered setting is absent from a rebuilt settings index.
Mechanism: index-completeness test over the settings schema.

## Appearance

[CUSTOM-009]
Status: Draft
The theme bridge uses `native-theme` and `native-theme-gpui` as the sole
source of the current system appearance.
Falsifier: appearance code reads portal settings or desktop theme files directly.
Mechanism: dependency check plus source scan for direct system-theme reads.

[CUSTOM-010]
Status: Draft
User theme overrides apply through named design tokens rather than direct
component colors.
Falsifier: a user color setting changes one component through a hardcoded color path.
Mechanism: theme-schema test plus source scan for configurable raw component colors.

[CUSTOM-011]
Status: Draft
The appearance page can reset all user theme overrides to the active
system-derived theme.
Falsifier: reset leaves any persisted user theme override active.
Mechanism: persistence test that changes every override and then resets.

## Actions and shortcuts

[CUSTOM-012]
Status: Draft
Every built-in operation exposed by menus, toolbar buttons, or shortcuts
is registered once in the command system with stable identity and state.
Falsifier: two entry points for the same action disagree on enabled or checked state.
Mechanism: command-registry test that compares every bound presentation.

[CUSTOM-013]
Status: Draft
Toolbar customization supports adding, removing, and reordering registered
commands while preserving commands that become temporarily unavailable.
Falsifier: a disabled or missing optional command is silently removed from the saved layout.
Mechanism: toolbar persistence test across capability and version changes.

[CUSTOM-014]
Status: Draft
Shortcut assignment detects active-scope collisions before saving a new
binding.
Falsifier: two commands in the same scope receive an indistinguishable shortcut.
Mechanism: shortcut-schema test over global, browser, and dialog scopes.

[CUSTOM-015]
Status: Draft
Custom actions declare their label, applicable MIME or location rules,
executable, argument vector, working-directory policy, and confirmation
policy before they can run. Arguments are substituted without a shell and
the environment is allowlisted.
Falsifier: a custom action executes for an unmatched selection or a hostile
name creates an extra argument, expansion, or environment leak.
Mechanism: parser and recording-runner tests with hostile names and environments.

## Context menus

[CUSTOM-016]
Status: Draft
Context menus are built from the command registry for the exact target:
file, directory, multi-selection, view background, sidebar location,
mount, archive, tag, or trash item. Menu actions share enabled state,
confirmation, and execution with toolbar and keyboard entry points.
Falsifier: a context action bypasses the registered command or uses a
different capability decision.
Mechanism: command-identity tests over every target kind.

[CUSTOM-017]
Status: Draft
File and directory menus include applicable Open, Open With, Send To, Cut,
Copy, Copy To, Move To, Paste Into, Rename, Duplicate, Create Symbolic
Link (soft link), Create Hard Link, Compress, Extract/Uncompress, Hide or
Unhide, Move to Trash, Delete Permanently,
Properties, Permissions, Open as Administrator, and Run as Administrator
commands. Inapplicable commands are absent; capability-limited commands
remain disabled with the reason when useful.
Falsifier: an applicable listed command is missing, an inapplicable command
can run, or the menu substitutes a different operation.
Mechanism: menu-schema matrix across file types, selections, stores, and capabilities.

[CUSTOM-018]
Status: Draft
Open With lists compatible applications, offers Choose Application, and
separates one-time launch from Set as Default; changing the association
requires the user to select Set as Default explicitly.
Falsifier: choosing an app once changes the MIME association or an
incompatible app is presented as recommended.
Mechanism: menu and association tests over known, unknown, and mixed MIME selections.

[CUSTOM-019]
Status: Draft
Send To lists writable pinned locations, mounted removable volumes, and
configured remote destinations; choosing one queues a copy and never
implies move or delete.
Falsifier: a read-only destination is enabled or Send To removes the source.
Mechanism: destination-model and operation-dispatch tests.

[CUSTOM-020]
Status: Draft
Hide renames each visible local item by adding one leading dot; Unhide
removes one leading dot. Both use rename conflict handling, preserve the
remaining name exactly, and are unavailable on stores without dot-name
semantics.
Falsifier: hide changes content, strips more than one dot, overwrites a
conflict, or runs on an incompatible store.
Mechanism: rename tests for files, directories, dotfiles, conflicts, and remote stores.

[CUSTOM-021]
Status: Draft
Permissions opens the selected item's Properties dialog on its Permissions
page; it does not perform a mutation from the menu itself.
Falsifier: choosing Permissions changes metadata before the user reviews and submits it.
Mechanism: context-menu routing test plus mutation-spy assertion.

[CUSTOM-022]
Status: Draft
Open as Administrator is available for local directories and opens a
visibly elevated, location-scoped browser surface. Run as Administrator is
available only for a local executable and shows its path and arguments
before requesting authorization. The confirmation names whether Polkit or
sudo will authorize it. Neither action starts the normal Musheen application
process as root.
Falsifier: an unsupported target exposes either action, authorization runs
without review, or the ordinary application process gains root identity.
Mechanism: target-matrix, authorization-cancellation, process-identity, and
hostile-argument tests.

[CUSTOM-023]
Status: Draft
The directory-background menu includes New Directory, New Empty File, New
from Template, Paste, Select All, Open Terminal Here, Show Hidden, view,
sort, group, and directory Properties commands when applicable.
Falsifier: a background command acts on a stale selection or bypasses the
active pane's location and capability state.
Mechanism: background-menu schema and dispatch tests in both panes.

[CUSTOM-024]
Status: Draft
A directory target additionally offers Open in New Tab, Open in New
Window, Open in Other Pane, Pin or Unpin, Copy Location, Open Terminal Here,
tags, and sharing when a provider supplies sharing support.
Falsifier: a navigation command changes the source unexpectedly or a
provider-specific command appears without its capability.
Mechanism: directory-menu matrix over local, mounted, archive, and remote stores.

[CUSTOM-025]
Status: Draft
A file target additionally offers Preview, Copy Location, tags, and
file-type actions. Archive files offer Browse and Extract Here; selected
non-archive items offer Compress; executable files offer Run according to
the executable-file preference.
Falsifier: an archive action appears for an unsupported type or executable
content runs contrary to the saved preference.
Mechanism: MIME, archive, and executable-policy menu tests.

[CUSTOM-026]
Status: Draft
Mount menus offer Open, Open in New Tab or Window, Unmount, Eject, Power
Off, and Properties according to UDisks2 capabilities. Trash menus offer
Restore and Delete Permanently, while Trash background offers Empty Trash.
Falsifier: a destructive or hardware action appears without the capability
or required confirmation.
Mechanism: mount and Trash menu matrices with mocked capabilities.

[CUSTOM-027]
Status: Draft
Menus order commands into Open, navigation, clipboard, creation, file-type,
organization, destructive, and details groups. Destructive commands remain
separated from ordinary commands and nested submenus are used for variable
lists such as Open With, Send To, Tags, and custom actions.
Falsifier: provider or extension contributions can insert an action into the
destructive group or cause unbounded top-level menu growth.
Mechanism: menu ordering and contribution-limit tests.

[CUSTOM-028]
Status: Draft
User custom actions and optional script-directory actions appear in a
clearly labeled Actions submenu, inherit the no-shell argument rules, and
are disabled for remote selections unless the action explicitly supports
provider URIs.
Falsifier: a script is mixed with built-in commands, receives an undeclared
remote path, or bypasses argument and confirmation policy.
Mechanism: local, remote, hostile-name, and oversized-contribution tests.

## Tags, pins, and home

[CUSTOM-029]
Status: Draft
The tag service presents one tag model whether the active store uses
extended attributes or app-owned fallback metadata.
Falsifier: the UI must branch on the tag storage backend to list or edit tags.
Mechanism: tag contract tests against xattr and fallback test providers.

[CUSTOM-030]
Status: Draft
Tag assignment preserves file identity across app-driven rename and move
operations whenever the destination store supports tags.
Falsifier: an app-driven supported move silently drops assigned tags.
Mechanism: integration tests for rename, same-store move, cross-store move,
and unsupported destination.

[CUSTOM-031]
Status: Draft
Fallback tags bind to provider identity plus stable item identity, update
after app-driven moves, mark externally missing items as orphaned, and
offer reviewed orphan cleanup without path guessing.
Falsifier: a recycled display path inherits another item's tags or cleanup
deletes a live tag record.
Mechanism: identity reuse, external rename, disappearance, and cleanup tests.

[CUSTOM-032]
Status: Draft
Pinned locations retain their user label and order while surfacing a clear
unavailable state when their target cannot be reached.
Falsifier: an unavailable pin disappears or blocks the rest of the pinned list.
Mechanism: pinned-location test with removed local and offline remote targets.

[CUSTOM-033]
Status: Draft
The home surface composes recent locations, pinned locations, mounts, and
tag shortcuts from their owning models without duplicating persistence.
Falsifier: changing a home item does not update the owning sidebar or settings model.
Mechanism: model identity tests for each home section.

[CUSTOM-034]
Status: Draft
The General settings page can disable recent-location recording and clear
existing history without removing pins, tags, or browsing-session restore data.
Falsifier: disabling or clearing history mutates another model or records a
new recent location afterward.
Mechanism: privacy-setting and model-isolation tests.

[CUSTOM-035]
Status: Draft
Copy To and Move To open the standard location chooser, preflight the
selected destination, and queue the same copy or move operation used by
drag, clipboard, and Send To. Cancelling the chooser performs no mutation.
Falsifier: either command bypasses conflict handling, capability checks, or
the operation queue.
Mechanism: chooser cancellation, provider capability, conflict, and command-identity tests.
