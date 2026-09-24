Status: Draft
Prefix: SYS

# Linux system integration

Desktop glue owns mounts, trash integration, MIME resolution, application
launching, D-Bus, portals, notifications, terminal integration, and remote
connections. It exposes portable traits to the rest of the app.

Review (2026-09-19): checked shell injection, credential exposure,
blocking I/O, stale mount state, sandbox boundaries, and remote failure.
The mechanisms cover both normal desktop services and service absence.

## Mounts and volumes

[SYS-001]
Status: Draft
The mount service observes mounted volumes and capacity data through
`proc-mounts` and `nix` behind one app-owned trait.
Falsifier: browser or sidebar code parses the mount table directly.
Mechanism: dependency-boundary check plus mocked mount-table contract test.

[SYS-002]
Status: Draft
The mount service emits add, remove, capacity, and availability changes to
the shared model without requiring a window refresh.
Falsifier: a mounted or removed fixture remains stale until manual refresh.
Mechanism: integration test that changes a mocked mount table while a window is open.

[SYS-003]
Status: Draft
Unmount, eject, and drive power-off call UDisks2 over D-Bus, surface its
authorization and busy errors, and refuse while app-owned operations use
the mount unless the user chooses cancellation first.
Falsifier: a volume action edits the mount table directly or invalidates an
active operation without a choice.
Mechanism: mocked UDisks2 tests for success, authorization, busy, unsupported,
and cancellable-operation paths.

## MIME and application launch

[SYS-004]
Status: Draft
The MIME service asks `xdg-mime` by name first and, when needed, through a
bounded content prefix. It asks `tree_magic_mini` only when the primary
result is unknown or generic binary, and never replaces a specific primary result.
Falsifier: ordinary named detection reads content or a fallback replaces a
specific primary result.
Mechanism: provider-spy tests over known, unknown, empty, misleading, and large files.

[SYS-005]
Status: Draft
The default-app resolver evaluates the freedesktop `mimeapps.list` cascade
in its specified precedence order.
Falsifier: a lower-precedence association wins over a valid higher-precedence entry.
Mechanism: layered `mimeapps.list` fixture test covering defaults, additions, and removals.

[SYS-006]
Status: Draft
The app parses desktop entries and resolves icons through the selected
`freedesktop` workspace crates behind replaceable app-owned traits.
Falsifier: a caller depends on a crate-specific desktop entry or icon type.
Mechanism: public-type boundary check plus provider contract tests.

[SYS-007]
Status: Draft
Application launch passes file arguments without shell evaluation and
honors the selected desktop entry's declared argument placeholders.
Falsifier: a file name can introduce an extra command, argument, or shell expansion.
Mechanism: hostile-filename launch tests with a recording process runner.

[SYS-008]
Status: Draft
The Open With flow lists compatible applications, lets the user make a
one-time choice, and offers an explicit default-association change.
Falsifier: a one-time launch changes the persisted default association.
Mechanism: interaction and resolver tests for one-time and make-default paths.

## Desktop services

[SYS-009]
Status: Draft
The single-instance D-Bus service implements FileManager1 `ShowFolders`,
`ShowItems`, and `ShowItemProperties`, accepts valid file URIs and startup
IDs, focuses the existing app, and routes properties requests to the same
Properties dialogs as the browser.
Falsifier: a valid FileManager1 request cannot focus the requested item or surface.
Mechanism: isolated-session-bus integration tests for each method.

[SYS-010]
Status: Draft
The D-Bus service returns a bounded error for malformed or unreachable
locations without terminating the app.
Falsifier: one invalid request crashes the service or never receives a reply.
Mechanism: malformed-request tests with response timeouts.

[SYS-011]
Status: Draft
As a portal client, Musheen uses `ashpd` FileChooser whenever it runs in a
sandbox or needs a portal-granted handle.
Falsifier: a sandboxed selection returns a path the caller cannot access.
Mechanism: portal integration test with a sandboxed caller fixture.

[SYS-012]
Status: Draft
Background operations send completion or failure notifications through
`notify-rust` only when no visible app window can show the result within
two seconds.
Falsifier: a foreground operation produces a duplicate desktop notification.
Mechanism: notification policy tests across focused, hidden, and closed-window states.

[SYS-013]
Status: Draft
Background maintenance rotates logs and performs user-enabled update checks
without delaying application startup. Update metadata must be HTTPS-fetched,
cryptographically signed by a pinned project key, and never auto-install. The
signed v2 payload binds its release channel and monotonic sequence. Musheen
accepts only its selected channel, stores the highest seen sequence in private
atomic state, and rejects older signed metadata after restart. The same valid
sequence may be offered again if delivery failed. Legacy and unsigned metadata
cannot produce an update offer.
Falsifier: maintenance blocks first-window readiness, runs a disabled check,
or presents unsigned, wrong-channel, or rolled-back metadata as an update.
Mechanism: startup timing and update-policy tests with delayed, disabled,
tampered, expired, replayed, wrong-channel, and valid providers.

## Terminal

[SYS-014]
Status: Draft
Open-in-terminal launches the configured terminal at the active local
directory without interpolating that directory into a shell command.
Falsifier: a directory name changes the launched command or terminal arguments.
Mechanism: hostile-path test with a recording process runner.

[SYS-015]
Status: Draft
The optional embedded terminal follows the focused pane's local directory
only after the user enables follow mode.
Falsifier: navigation changes an embedded terminal directory while follow mode is off.
Mechanism: terminal interaction test across pane focus and follow-mode changes.

[SYS-016]
Status: Draft
The embedded terminal runs the configured user shell in a pseudoterminal
owned by its window and keeps that session alive while its drawer is hidden.
Falsifier: hiding and reopening the drawer starts a new shell or loses terminal job control.
Mechanism: pseudoterminal integration test that hides and restores a shell
with exported state and a suspended foreground job.

[SYS-017]
Status: Draft
Closing a window with an active foreground terminal job requires an explicit
choice before the app terminates that terminal's process group.
Falsifier: closing the window silently terminates an active foreground job.
Mechanism: window-close interaction test with idle, running, and suspended jobs.

[SYS-018]
Status: Draft
The terminal emulator bounds scrollback by the configured line count and
memory ceiling, sanitizes clipboard control sequences, ignores unsupported
device-control requests, and requires user action before opening a terminal URL.
Falsifier: terminal output can write the clipboard, trigger a host action,
or grow scrollback beyond either bound without consent.
Mechanism: hostile escape-sequence, OSC URL, clipboard, and scrollback tests.

[SYS-019]
Status: Draft
Terminal paste uses bracketed paste when requested, warns before multiline
or control-character paste, and never interprets pasted text in the UI layer.
Falsifier: a multiline or control-character paste reaches the PTY without
warning or is modified into executable shell syntax by Musheen.
Mechanism: recording-PTY paste tests for plain, multiline, control, and bracketed modes.

## Remote locations

[SYS-020]
Status: Draft
The remote store exposes FTP, FTPS, SFTP, WebDAV, and HTTP through OpenDAL
and exposes SMB through `pavao`.
Falsifier: a supported protocol bypasses its assigned backend.
Mechanism: dependency-boundary check plus provider construction tests per scheme.

[SYS-021]
Status: Draft
SMB calls run through bounded blocking workers rather than an asynchronous
executor thread.
Falsifier: a blocking SMB call stalls an unrelated asynchronous operation.
Mechanism: concurrency test with a deliberately blocked SMB fixture.

[SYS-022]
Status: Draft
NFS locations use kernel mounts and never create a parallel userspace NFS
client.
Falsifier: the dependency graph or source tree contains NFS wire-protocol client code.
Mechanism: dependency and source checks plus mounted-NFS provider test.

[SYS-023]
Status: Draft
Remote operations apply bounded connection timeouts and expose retry only
when repeating the operation cannot duplicate a completed mutation.
Falsifier: a remote request waits without a deadline or blindly retries a non-idempotent mutation.
Mechanism: timeout and retry-policy tests for reads, creates, moves, and deletes.

[SYS-024]
Status: Draft
Remote credentials live in the desktop secret service and never appear in
URLs, settings files, logs, notifications, or error text.
Falsifier: a captured credential appears in any listed output or persisted location.
Mechanism: credential-flow integration test with captured storage and diagnostic outputs.

[SYS-025]
Status: Draft
Each remote provider maps its actual read, write, rename, link, trash,
watch, metadata, and atomic-publication support into the core capability
matrix before browser commands are enabled.
Falsifier: a remote command is enabled from protocol name alone and then
fails for a known unsupported capability.
Mechanism: provider contract fixtures per supported protocol and server capability set.

[SYS-026]
Status: Draft
If the desktop secret service is unavailable or locked, Musheen may use a
session-only credential after consent but never stores it in settings or URLs.
Falsifier: secret-service failure silently writes a credential elsewhere or
prevents the user from choosing session-only use.
Mechanism: locked, missing, cancelled, session-only, and restored-service tests.

## Portals and privilege

[SYS-027]
Status: Draft
When enabled in Integrations settings, Musheen serves the FileChooser
portal backend through `ashpd::backend`, returns only user-confirmed
selections, and remains separate from its portal-client path.
Falsifier: a caller obtains an unconfirmed path or a client request loops
back into Musheen's own backend.
Mechanism: isolated-bus backend tests for open, save, cancel, sandbox grant,
and self-routing prevention.

[SYS-028]
Status: Draft
Privileged browsing and mutations use a minimal authorization broker that
validates the requested local path and operation on every call, drops unused
environment and file descriptors, and never reads user settings or secrets.
Falsifier: authorization grants an unrestricted root application session or
the broker accepts a path or operation not shown to the user.
Mechanism: broker protocol, confused-deputy, environment, descriptor, and
authorization-cancellation tests.

[SYS-029]
Status: Draft
Run as Administrator passes a canonical local executable and explicit
argument vector to the broker without shell evaluation. Open as
Administrator grants only the selected local directory and displays a
persistent elevated-state banner on that surface.
Falsifier: a file name adds a command, an elevated surface escapes its
granted root without new authorization, or elevated state is not visible.
Mechanism: hostile-path, scope-escape, symlink-swap, and elevated-UI tests.

[SYS-030]
Status: Draft
Integrations settings selects Polkit as the default privilege provider or
sudo as an explicit alternative. The sudo provider runs only the broker as
an argument vector using `sudo --`, obtains authentication through a
dedicated PTY, and never stores, echoes, logs, or substitutes the password.
Falsifier: sudo executes a shell-built command, elevates the ordinary app,
or exposes authentication input outside the dedicated PTY.
Mechanism: recording-sudo, hostile-argument, cancelled-authentication,
wrong-password, timeout, and captured-output tests.
