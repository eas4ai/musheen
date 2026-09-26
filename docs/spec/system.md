Prefix: SYS

# Linux system integration

Desktop glue owns mounts, trash integration, MIME resolution, application
launching, D-Bus, portals, notifications, terminal integration, and remote
connections. It exposes portable traits to the rest of the app.

Review (2026-09-19): checked shell injection, credential exposure,
blocking I/O, stale mount state, sandbox boundaries, and remote failure.
The mechanisms cover both normal desktop services and service absence.

## Mounts and volumes

[SYS-001] The mount service observes mounted volumes and capacity data through `proc-mounts` and `nix` behind one app-owned trait.
Falsifier: browser or sidebar code parses the mount table directly.
Mechanism: dependency-boundary check plus mocked mount-table contract test.
Status: Draft

[SYS-002] The mount service emits add, remove, capacity, and availability changes to the shared model without requiring a window refresh.
Falsifier: a mounted or removed fixture remains stale until manual refresh.
Mechanism: integration test that changes a mocked mount table while a window is open.
Status: Draft

[SYS-003] Unmount, eject, and drive power-off call UDisks2 over D-Bus, surface its authorization and busy errors, and refuse while app-owned operations use the mount unless the user chooses cancellation first.
Falsifier: a volume action edits the mount table directly or invalidates an active operation without a choice.
Mechanism: mocked UDisks2 tests for success, authorization, busy, unsupported, and cancellable-operation paths.
Status: Draft

## MIME and application launch

[SYS-004] The MIME service asks `xdg-mime` by name first and, when needed, through a bounded content prefix. It asks `tree_magic_mini` only when the primary result is unknown or generic binary, and never replaces a specific primary result.
Falsifier: ordinary named detection reads content or a fallback replaces a specific primary result.
Mechanism: provider-spy tests over known, unknown, empty, misleading, and large files.
Status: Draft

[SYS-005] The default-app resolver evaluates the freedesktop `mimeapps.list` cascade in its specified precedence order.
Falsifier: a lower-precedence association wins over a valid higher-precedence entry.
Mechanism: layered `mimeapps.list` fixture test covering defaults, additions, and removals.
Status: Draft

[SYS-006] The app parses desktop entries and resolves icons through the selected `freedesktop` workspace crates behind replaceable app-owned traits.
Falsifier: a caller depends on a crate-specific desktop entry or icon type.
Mechanism: public-type boundary check plus provider contract tests.
Status: Draft

[SYS-007] Application launch passes file arguments without shell evaluation and honors the selected desktop entry's declared argument placeholders.
Falsifier: a file name can introduce an extra command, argument, or shell expansion.
Mechanism: hostile-filename launch tests with a recording process runner.
Status: Draft

[SYS-008] The Open With flow lists compatible applications, lets the user make a one-time choice, and offers an explicit default-association change.
Falsifier: a one-time launch changes the persisted default association.
Mechanism: interaction and resolver tests for one-time and make-default paths.
Status: Draft

## Desktop services

[SYS-009] The single-instance D-Bus service implements FileManager1 `ShowFolders`, `ShowItems`, and `ShowItemProperties`, accepts valid file URIs and startup IDs, focuses the existing app, and routes properties requests to the same Properties dialogs as the browser.
Falsifier: a valid FileManager1 request cannot focus the requested item or surface.
Mechanism: isolated-session-bus integration tests for each method.
Status: Draft

[SYS-010] The D-Bus service returns a bounded error for malformed or unreachable locations without terminating the app.
Falsifier: one invalid request crashes the service or never receives a reply.
Mechanism: malformed-request tests with response timeouts.
Status: Draft

[SYS-011] As a portal client, Musheen uses `ashpd` FileChooser whenever it runs in a sandbox or needs a portal-granted handle.
Falsifier: a sandboxed selection returns a path the caller cannot access.
Mechanism: portal integration test with a sandboxed caller fixture.
Status: Draft

[SYS-012] Background operations send completion or failure notifications through `notify-rust` only when no visible app window can show the result within two seconds.
Falsifier: a foreground operation produces a duplicate desktop notification.
Mechanism: notification policy tests across focused, hidden, and closed-window states.
Status: Draft

[SYS-013] Background maintenance rotates logs and performs user-enabled update checks without delaying application startup. Update metadata must be HTTPS-fetched, cryptographically signed by a pinned project key, and never auto-install. The signed v2 payload binds its release channel and monotonic sequence. Musheen accepts only its selected channel, stores the highest seen sequence in private atomic state, and rejects older signed metadata after restart. The same valid sequence may be offered again if delivery failed. Legacy and unsigned metadata cannot produce an update offer.
Falsifier: maintenance blocks first-window readiness, runs a disabled check, or presents unsigned, wrong-channel, or rolled-back metadata as an update.
Mechanism: startup timing and update-policy tests with delayed, disabled, tampered, expired, replayed, wrong-channel, and valid providers.
Status: Draft

## Terminal

[SYS-014] Open-in-terminal launches the configured terminal at the active local directory without interpolating that directory into a shell command.
Falsifier: a directory name changes the launched command or terminal arguments.
Mechanism: hostile-path test with a recording process runner.
Status: Draft

[SYS-015] The optional embedded terminal follows the focused pane's local directory only after the user enables follow mode.
Falsifier: navigation changes an embedded terminal directory while follow mode is off.
Mechanism: terminal interaction test across pane focus and follow-mode changes.
Status: Draft

[SYS-016] The embedded terminal runs the configured user shell in a pseudoterminal owned by its window and keeps that session alive while its drawer is hidden.
Falsifier: hiding and reopening the drawer starts a new shell or loses terminal job control.
Mechanism: pseudoterminal integration test that hides and restores a shell with exported state and a suspended foreground job.
Status: Draft

[SYS-017] Closing a window with an active foreground terminal job requires an explicit choice before the app terminates that terminal's process group.
Falsifier: closing the window silently terminates an active foreground job.
Mechanism: window-close interaction test with idle, running, and suspended jobs.
Status: Draft

[SYS-018] The terminal emulator bounds scrollback by the configured line count and memory ceiling, sanitizes clipboard control sequences, ignores unsupported device-control requests, and requires user action before opening a terminal URL.
Falsifier: terminal output can write the clipboard, trigger a host action, or grow scrollback beyond either bound without consent.
Mechanism: hostile escape-sequence, OSC URL, clipboard, and scrollback tests.
Status: Draft

[SYS-019] Terminal paste uses bracketed paste when requested, warns before multiline or control-character paste, and never interprets pasted text in the UI layer.
Falsifier: a multiline or control-character paste reaches the PTY without warning or is modified into executable shell syntax by Musheen.
Mechanism: recording-PTY paste tests for plain, multiline, control, and bracketed modes.
Status: Draft

## Remote locations

[SYS-020] The remote store exposes FTP, FTPS, SFTP, WebDAV, and HTTP through OpenDAL and exposes SMB through `pavao`.
Falsifier: a supported protocol bypasses its assigned backend.
Mechanism: dependency-boundary check plus provider construction tests per scheme.
Status: Draft

[SYS-021] SMB calls run through bounded blocking workers rather than an asynchronous executor thread.
Falsifier: a blocking SMB call stalls an unrelated asynchronous operation.
Mechanism: concurrency test with a deliberately blocked SMB fixture.
Status: Draft

[SYS-022] NFS locations use kernel mounts and never create a parallel userspace NFS client.
Falsifier: the dependency graph or source tree contains NFS wire-protocol client code.
Mechanism: dependency and source checks plus mounted-NFS provider test.
Status: Draft

[SYS-023] Remote operations apply bounded connection timeouts and expose retry only when repeating the operation cannot duplicate a completed mutation.
Falsifier: a remote request waits without a deadline or blindly retries a non-idempotent mutation.
Mechanism: timeout and retry-policy tests for reads, creates, moves, and deletes.
Status: Draft

[SYS-024] Remote credentials live in the desktop secret service and never appear in URLs, settings files, logs, notifications, or error text. The connection editor takes a connection's password. Test connection uses the password typed, or the stored one when the field is empty. Saving stores the password in the secret service under the connection's ID and writes only its reference to the settings file; browsing the saved connection logs in with it; saving with the password field empty keeps the stored password; and removing the connection deletes it from the secret service.
Falsifier: Test connection or the saved connection's browse login does not receive the password typed in the editor, saving with the password field empty loses the stored password, the password stays in the secret service after its connection is removed, or the password appears in the settings file, a connection URL, an error text, or the debug rendering of a profile or credential.
Mechanism: sys-024
Rationale: 1.x mechanism: credential-flow integration test with captured storage and diagnostic outputs; docs/opus-audit-2.md 4.5: nothing in the app could store a password, so a saved connection that needs one could not log in unless another tool had put it in the keyring.
Status: Agreed 2026-09-26

[SYS-025] Each remote provider maps its actual read, write, rename, link, trash, watch, metadata, and atomic-publication support into the core capability matrix before browser commands are enabled.
Falsifier: a remote command is enabled from protocol name alone and then fails for a known unsupported capability.
Mechanism: provider contract fixtures per supported protocol and server capability set.
Status: Draft

[SYS-026] If the desktop secret service is unavailable or locked when a connection is saved, the connection editor offers session-only use of its password. After the user chooses it, the password stays in memory until Musheen exits, serves Test connection and browsing, and is never written to settings, URLs, or any file.
Falsifier: a secret-service failure silently writes a credential elsewhere or leaves the user no session-only choice, or a password chosen for session-only use does not reach the saved connection's browse login or is written to the settings file.
Mechanism: sys-026
Rationale: 1.x mechanism: locked, missing, cancelled, session-only, and restored-service tests.
Status: Agreed 2026-09-26

[SYS-031] Test connection passes only when browsing can open the connection and list its root, using the same host, port, TLS mode, and credentials, and a failed test names its cause. The connection editor offers only the protocols and settings browsing can open; FTPS uses explicit TLS (AUTH TLS) on port 21 unless the connection names another port. A saved connection that browsing cannot open stays in Settings with the reason and is not listed under Network.
Falsifier: a connection passes Test connection while browsing cannot open it or list its root, or fails the test while browsing can; the editor offers a protocol or setting that browsing refuses; a failed test does not name its cause; or Network lists a saved connection that browsing refuses.
Mechanism: sys-031
Rationale: docs/opus-audit-2.md 4.5: the FTPS test used implicit TLS on port 990 while browsing sent AUTH TLS, every browse store refused the proxies the editor offered, and the SFTP test always failed.
Status: Agreed 2026-09-26

[SYS-032] An SFTP connection logs in with the method its profile selects: a password, the keys held by the running SSH agent, a private key file, or a private key stored in the secret service. An encrypted key's passphrase is kept like a password under SYS-024 and SYS-026. An RSA key is used only through the SSH agent, which is asked for a SHA-2 signature; a private key file or stored key must be Ed25519 or ECDSA, and an RSA one is refused with a message that points to the agent. When the connection's host names a Host entry in ~/.ssh/config, that entry's HostName and ProxyJump apply, and its User, Port, and IdentityFile fill the fields the profile leaves empty. The server's key must pass the connection's host-key policy, and each jump host's key must match known_hosts.
Falsifier: an SFTP login does not offer the selected password, agent key, or Ed25519 or ECDSA private key to a server that accepts it; an agent-held RSA key is asked for a SHA-1 signature; Musheen itself signs with an RSA key file or stored RSA key; a passphrase or a stored key is written anywhere but the secret service; a Host entry's HostName, User, Port, IdentityFile, or ProxyJump is ignored; or a connection goes through a jump host whose key does not match known_hosts.
Mechanism: sys-032
Rationale: the developer asked for key login on 2026-09-26 and ruled RSA to the agent only, because the rsa crate carries the unpatched timing advisory RUSTSEC-2023-0071; the SFTP code accepted only an unencrypted key placed in the keyring by another tool and read ~/.ssh/config only on a system-ssh path that the test never used.
Status: Agreed 2026-09-26

[SYS-033] Each saved connection names its own credential, and Settings has no global remote credential. A settings file that still holds the global value from an older build loads with every setting and connection intact and keeps that value unread. A connection that refers to that secret still logs in with it.
Falsifier: Settings shows, searches or edits a global remote credential; a settings file that holds one fails to load or loses a setting or a connection; or a connection that refers to the old global secret no longer logs in, or loses its reference when saved.
Mechanism: sys-033
Rationale: since remote-usable-extract each connection stores its own password under its ID, so the global remote.credential setting was shown on the Integrations page but read by nothing (adversary report 4c4ba543 finding 18, backlog item retire-global-remote-credential); the developer agreed this text on 2026-09-26.
Status: Agreed 2026-09-26

## Portals and privilege

[SYS-027] When enabled in Integrations settings, Musheen serves the FileChooser portal backend through `ashpd::backend`, returns only user-confirmed selections, and remains separate from its portal-client path.
Falsifier: a caller obtains an unconfirmed path or a client request loops back into Musheen's own backend.
Mechanism: isolated-bus backend tests for open, save, cancel, sandbox grant, and self-routing prevention.
Status: Draft

[SYS-028] Privileged browsing and mutations use a minimal authorization broker that validates the requested local path and operation on every call, drops unused environment and file descriptors, and never reads user settings or secrets.
Falsifier: authorization grants an unrestricted root application session or the broker accepts a path or operation not shown to the user.
Mechanism: broker protocol, confused-deputy, environment, descriptor, and authorization-cancellation tests.
Status: Draft

[SYS-029] Run as Administrator passes a canonical local executable and explicit argument vector to the broker without shell evaluation. Open as Administrator grants only the selected local directory and displays a persistent elevated-state banner on that surface.
Falsifier: a file name adds a command, an elevated surface escapes its granted root without new authorization, or elevated state is not visible.
Mechanism: hostile-path, scope-escape, symlink-swap, and elevated-UI tests.
Status: Draft

[SYS-030] Integrations settings selects Polkit as the default privilege provider or sudo as an explicit alternative. The sudo provider runs only the broker as an argument vector using `sudo --`, obtains authentication through a dedicated PTY, and never stores, echoes, logs, or substitutes the password.
Falsifier: sudo executes a shell-built command, elevates the ordinary app, or exposes authentication input outside the dedicated PTY.
Mechanism: recording-sudo, hostile-argument, cancelled-authentication, wrong-password, timeout, and captured-output tests.
Status: Draft
