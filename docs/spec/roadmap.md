
# Roadmap

The roadmap orders implementation so later UI and operations build on
proved storage and command boundaries. A commitment advances only when its
listed requirements have executable checks and those checks pass.

Current: compact-broker-listings

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

## local-safety-index-2026-09-25

Requirements: OPS-019, OPS-021, OPS-028, BROWSE-019, BROWSE-023

Fix the local data-safety and large-folder findings from docs/opus-audit-2.md
sections 5.1 and 5.2: the no-replace fallback that leaves a partial tree
under the destination name (OPS-019), sparse files expanded silently inside
copied or moved folders (OPS-021), Trash restore of a directory link and a
listing that fails on one orphaned entry (OPS-028), and the directory index
that lives in the temp dir, leaks on SIGTERM, drops the shown items when its
first write fails, collapses the selection on Ctrl-click after Select All,
and leaves the rubber band and the Columns layout inert (BROWSE-019,
BROWSE-023). Done when each mechanism's test fails on the recorded violating
example and passes on the fix, the workspace tests and clippy are clean, and
the review and report are accepted.

## ui-thread-growth-2026-09-25

Requirements: UXF-023, BROWSE-020, UIV-014, OPS-013, LIMIT-010

Fix the UI-thread and growth findings from docs/opus-audit-2.md sections
5.1 and 5.2: catalog file I/O on every watch event and stat and statfs on
every context menu and command target check (UXF-023), the whole-order
rebuild on every watch event in an indexed directory and its silent
failure (BROWSE-020), the fallback theme that always ends on light
(UIV-014), the full trash listing on every finished trash job and undo
check (OPS-013), and the scheduler records, status history and persisted
status document that grow without bound (LIMIT-010). Done when each
mechanism's test fails on the recorded violating example and passes on
the fix, the workspace tests and clippy are clean, and every finding of
the review and the report is resolved or declined.

## catalog-writes-off-ui-thread

Requirements: UXF-023

Take the remaining catalog file I/O and the store calls that come with it
off the UI thread (backlog item catalog-writes-off-ui-thread, escalation
54ca2f0b). The item names Properties tag edits and per-directory view
preferences; the same wait is also in navigation, which resolves the
folder identity and records recents and the remembered location on every
load; in sidebar tag rename and delete, which ask each tagged path's
capabilities; in the catalog projection sync, which resolves every pin
and rewrites the catalog on each catalog change; in Home orphan cleanup;
and in the move and rename completion, which asks statfs and rewrites
the catalog. The resolutions of review 75af775d finding 1 and report
9fa1a7b3 finding 5 said a09935f moved the move completion off the UI
thread; it did not, and this commitment does. The initial catalog read
when a window is built stays where it is. Done when a test for each of
these paths holds the catalog lock from another open file or blocks the
store and the window still repaints and takes input, the paths still
apply their change once the lock or the store is released, the workspace
tests and clippy are clean, and every finding of the review and the
report is resolved or declined.

## menu-composition-state

Requirements: UXF-023

Fold the menu composition state into one record (backlog item
menu-composition-state, adversary report 9fa1a7b3 finding 12). The probes
a menu queues, how to compose that menu again, the probes queued outside a
menu and the probes in flight now live in five fields: composing_menu,
pending_store_probes, pending_menu_recompose, render_probes and
probes_in_flight. Three paths take or move them: take_pending_menu_probe,
move_menu_probes_to_render and flush_render_probes. A composition that
does not take its probes leaves them, with its recompose record, for the
next menu, and several callers compose a menu without taking them. A menu
composition now opens a record and closes it when it ends; what it queued
either goes to the popup that waits for it or to the next frame's probes,
so nothing is left for another menu. Done when that record replaces the
five fields, a test shows that a menu composed without taking its probes
leaves no probe and no recompose record for the next menu, the UXF-023
tests still pass, the workspace tests and clippy are clean, and every
finding of the review and the report is resolved or declined.

## fallback-theme-follows-appearance-changes

Requirements: UIV-014

Make the fallback theme follow the window appearance after startup
(backlog item fallback-theme-follows-appearance-changes, adversary report
9fa1a7b3 finding 11). When the native-theme bridge cannot read the system
theme, install_native_theme installs the Adwaita preset for the window
appearance once, at startup; a later switch between dark and light keeps
the old variant until the next start, so the fallback shows a variant the
window appearance did not request. Each browser window now observes its
appearance and, while the fallback is in use, installs the variant the
new appearance asks for and applies the appearance settings again. When
the system theme becomes readable later, the fallback stops following, so
an appearance change never replaces a system theme with Adwaita. Done when
a test switches a window's appearance while the fallback is in use and
sees the matching variant, a test shows the fallback does nothing when the
system theme is in use, the UIV-014 check passes, the workspace tests,
cargo fmt --check and clippy with warnings denied are clean, and every
finding of the review and the report is resolved or declined.

## xattr-io-outside-catalog-queue

Requirements: UXF-024

Take extended-attribute I/O out of the catalog write queue (backlog item
xattr-io-outside-catalog-queue, review 54e960e8 finding 4). A tag edit's
attribute write, the attribute read and import behind the Properties tag
states, and the pending-attribute pass all run inside a queue job today,
so a tagged file on a hung mount holds every later catalog write, from
every window, until the mount answers. The attribute reads now run before
the job is queued, the job records the desired tags in the catalog, and
the attribute writes run one at a time in their own background lane; each
write that lands is finished in the catalog by another queued job, which
stages the tags again when they changed meanwhile. Done when a test blocks
one tagged file's attribute read or write and a later catalog write for
something else lands, the tags reach the file once it is unblocked, the
UXF-024 and UXF-023 checks pass, the workspace tests, cargo fmt --check
and clippy with warnings denied are clean, and every finding of the
review and the report is resolved or declined.

## remote-usable

Requirements: SYS-024, SYS-026, SYS-031, SYS-032, OPS-033, DEP-022

Make saved remote connections and Extract… usable (docs/opus-audit-2.md
4.5). The connection editor takes a password and, for SFTP, a login
method: password, SSH agent, key file, or a key stored in the secret
service. Passwords, passphrases and stored keys go to the secret service,
or stay in memory for the session when it is locked or missing, and a
Remove button deletes a connection with its secrets. Test connection opens
the connection through the code browsing uses and lists its root, and a
failed test names its cause. FTPS uses explicit TLS on port 21 unless the
connection names another port; the editor stops offering SMB, NFS,
proxies and the FTPS certificate pin, and Network hides saved connections
that use them. SFTP runs on russh only, uses RSA keys only through the SSH agent
with SHA-2 signatures, reads
~/.ssh/config through ssh2-config (Host, HostName, User, Port,
IdentityFile, ProxyJump, Include) and checks each jump host against
known_hosts. Extract… extracts into the picked folder. Done when each
mechanism's test fails on its recorded violating example and passes on
the final tree; the SFTP tests log in against an in-process SSH server,
agent and jump host with throwaway keys, and no test reads the user's
keyring or ~/.ssh; the workspace tests, cargo fmt --check and clippy with
warnings denied are clean; and every finding of the review and the report
is resolved or declined.

## remote-usable-extract

Requirements: SYS-024, SYS-026, SYS-031, SYS-032, OPS-033, DEP-022

Continue remote-usable, superseded when the developer revised OPS-033
(escalation 407ae943), with its work carried: passwords in the secret
service with session-only use and Remove, Test connection through the
browse connector with a named cause, FTPS as explicit TLS on port 21, no
SMB, NFS, HTTP, proxy or FTPS-pin choice in the editor or Network, and
SFTP on russh with the agent, key files, stored keys, RSA through the
agent only, and ~/.ssh/config through ssh2-config. Extract… and Extract
Here now extract into a folder named after the archive: published in one
step when absent; when present, the entries merge into it, folders merge
entry by entry, and each colliding file, or item of the other kind, asks
Replace, Replace All, Skip or Skip All before the job runs. The engine
follows those answers and skips a collision that appeared after the
question. This also fixes Extract Here, which failed on every archive
because its destination always existed. Done when each mechanism's
current receipt passes on the final tree (the OPS-033 review is bound to
fail receipt 75755a98 for the revised text), no test reads the user's
keyring or ~/.ssh, the workspace tests, cargo fmt --check and clippy with
warnings denied are clean, and every finding of the review and the report
is resolved or declined.

## retire-global-remote-credential

Requirements: SYS-033

Retire the global remote.credential setting (backlog item
retire-global-remote-credential, adversary report 4c4ba543 finding 18).
Each connection stores its own password under its ID, so the setting is
shown on the Integrations page and in settings search but read by nothing.
It leaves the settings schema, the page, settings search and the three
locales, with the document helpers that only it used. A settings file an
older build wrote with the value still loads: the document keeps an
unknown key as it is, and a connection that refers to that secret keeps
its reference and logs in with it. Done when the sys-033 check passes on
the final tree, the workspace tests, cargo fmt --check and clippy with
warnings denied are clean, and every finding of the review and the report
is resolved or declined.

## extraction-cost

Requirements: OPS-034, OPS-033

Make extraction cost what it needs (docs/opus-audit-2.md C-N3). The
extract operation reads the archive where it is instead of copying and
hashing it, lists and checks its entries in one pass, and decodes each
file once in archive order, so tar and 7z no longer decode from the
start of the archive for each entry. It checks that the archive did not
change before it publishes. An archive inside the archive is written as
a file and never opened, and the 30-second decoding limit goes. The
collision check before a merge lists the entries in one pass. Extract…
and Extract Here keep working as OPS-033 says. Done when the ops-034 and
ops-033 checks pass on the final tree, the workspace tests, cargo fmt
--check and clippy with warnings denied are clean, and every finding of
the review and the report is resolved or declined.

## elevated-session

Requirements: SYS-034

Give each elevated window one broker session (docs/opus-audit-2.md
C-N2). Open as Administrator authorizes once, with Polkit or sudo, and
starts a broker that stays with its window. It lists folders inside the
granted folder, checking each request against that folder and its
identity, and ends when the window closes, when Musheen exits, or after
15 minutes without a request. The transports read broker output as it
arrives, so a large listing no longer blocks on the pipe; the sudo
broker's terminal runs raw, so a long request is not cut; the window
keeps a fetched listing for its later pages; and the per-listing Polkit
browse action goes. Run as Administrator keeps one authorization per
run. Done when the sys-034 check passes on the final tree, the workspace
tests, cargo fmt --check and clippy with warnings denied are clean, and
every finding of the review and the report is resolved or declined.

## run-files

Requirements: SYS-035, SYS-036

Run only what may run, and run exactly the reviewed file
(docs/opus-audit-2.md C-N10). Musheen runs a local compiled program or
executable desktop entry only when the kernel lets the user execute it.
Double-click follows the executable-file preference: Open opens the
file, Ask reviews it with Run, Open and Cancel, and Run runs it at once;
the menu's Run always reviews first. A compiled program runs from the
file Musheen opened and checked; a desktop entry starts the program its
Exec line names; scripts and other files never run on double-click or
through Run. Run in Terminal runs a script or compiled program the user
may execute, in its own session in the terminal drawer or in the system
terminal as a new setting chooses, keeping its output and exit code and
asking before it closes or replaces a running one. The Settings window
can change the executable-file preference and the new setting. Done
when the sys-035 and sys-036 checks pass on the final tree, the
workspace tests, cargo fmt --check and clippy with warnings denied are
clean, and every finding of the review and the report is resolved or
declined.

## permissions-page

Requirements: SEARCH-019

Rebuild the Properties Permissions page like Dolphin's. Owner, Group and
Others each choose an access level; files get an Allow executing file as
program checkbox; the owner shows by name and the group is chosen by name
from the user's groups; Advanced Permissions shows the mode bits and the
ACL entries; Varies marks what a selection does not share; Apply to
contents follows the scope review; nothing changes before Apply, which
runs through the operations queue; No Access on the user's own file can
be undone; a filesystem without POSIX permissions gets a read-only page;
and every string is localized. Changing the owner as administrator and
editing ACL entries come later. Done when the search-019 check passes on
the final tree, the workspace tests, cargo fmt --check and clippy with
warnings denied are clean, and every finding of the review and the report
is resolved or declined.

## portal-backend

Requirements: SYS-027

Make Musheen's FileChooser portal backend safe and usable. It answers only
the portal service, refusing every other caller, Close included. Each
request opens a chooser window of its own: Open picks one file, several
files or a folder as the request asks; Save picks a folder and a name;
Save Many picks a folder for the requested names; both ask before they
replace a file; the request's filters are offered. Only a confirmed
selection is returned. The package ships musheen.portal and a D-Bus
activation file; started for a portal request, Musheen opens only the
chooser, and with the setting off it exits without a window. The package
installs no portals.conf. Done when the sys-027 check passes on the final
tree, the workspace tests, cargo fmt --check and clippy with warnings
denied are clean, and every finding of the review and the report is
resolved or declined.

## compact-broker-listings

Requirements: SYS-034

Let a listing of up to 64 MiB reach an elevated window whole (backlog
item compact-broker-listings, elevated-session report finding 5). The
broker sends each byte of a name as a JSON number, about four bytes for
each byte, so a folder whose names total about 16 MiB is refused. Names
and identities go as base64 text instead, so the 64 MiB bound counts
close to what the folder holds. The broker counts the listing's size as
it reads the folder and stops at 64 MiB without reading the rest, and
the window says that the folder is too large to list as administrator,
naming the limit, instead of a general failure. Done when the sys-034
check passes on the final tree, the workspace tests, cargo fmt --check
and clippy with warnings denied are clean, and every finding of the
review and the report is resolved or declined.
