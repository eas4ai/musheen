# Linux Desktop Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Integrate Musheen with Linux applications, volumes, desktop services, portals, privilege boundaries, and a Dolphin-style terminal drawer.

**Architecture:** `musheen-desktop` exposes narrow traits for each external service. Production adapters use D-Bus, portals, process launch, secret storage, and PTYs; deterministic fakes drive all policy tests. Privilege is per operation and the GUI never runs as root.

**Tech Stack:** freedesktop app/icon crates, custom mimeapps resolver, open, ashpd, zbus, notify-rust, secret-service, portable-pty, alacritty_terminal, UDisks2, Polkit, and GPUI Kit rendering.

---

**Primary requirements:** SYS-001–019, SYS-027–030; DEP-004, DEP-009,
DEP-016–017; CUSTOM-022; UXF-005, UXF-017; UIV-019, UIV-023; LIMIT-007.

### Task 1: Resolve MIME applications and Open With

**Files:** Create `crates/musheen-desktop/src/apps/{mod,desktop_entry,mimeapps,launch}.rs`;
create `crates/musheen-desktop/tests/{mimeapps,launch}.rs`; connect
`crates/musheen-ui/src/dialogs/open_with.rs`.

- [x] Test the full mimeapps.list precedence cascade, removed associations,
  defaults, desktop visibility, `%f/%F/%u/%U` expansion, invalid entries,
  terminal apps, non-UTF-8 path refusal, and association persistence.
- [x] Run the tests; expect failure.
- [x] Implement the custom resolver and freedesktop desktop-entry adapter.
  Launch with argv vectors, honor `TryExec`, never use shell interpolation, and
  report every item that cannot cross a URI/path boundary losslessly.
- [x] Make Open With support one-time choice and explicit default association.
- [x] Run malicious desktop-entry and argument fixtures.
- [x] Commit with `feat(desktop): add MIME application associations`.

### Task 2: Add mounts, devices, and UDisks2 operations

**Files:** Create `crates/musheen-desktop/src/volumes/{mod,model,udisks,mounts}.rs`;
modify sidebar; create `crates/musheen-desktop/tests/volumes.rs`.

- [x] Test insert/remove, mount, unmount, eject, unlock, busy errors, duplicate
  mount records, stale D-Bus objects, capability updates, and disappearance
  during an operation.
- [x] Run the volume test against a fake D-Bus service; expect failure.
- [x] Implement an event-driven volume model using UDisks2 through zbus and
  proc-mounts reconciliation. Never invoke command-line mount tools as fallback.
- [x] Route all actions through commands and reflect mounted/read-only/free-space
  changes in sidebar and Properties.
- [x] Run fake-service tests and one supported-desktop integration smoke test.
- [x] Commit with `feat(desktop): add volume and device integration`.

### Task 3: Export desktop services and portal behavior

**Files:** Create `crates/musheen-desktop/src/{file_manager1,portals,notifications,maintenance,updates}.rs`;
create `crates/musheen-desktop/tests/{file_manager1,portals,updates}.rs`.

- [x] Test ShowItems/ShowFolders/ShowItemProperties, activation into an existing
  window, malformed URIs, portal cancellation, document grants, optional
  FileChooser backend recursion prevention, notification actions, nonblocking
  log rotation, disabled/delayed update checks, and tampered/expired/valid
  signed update metadata.
- [x] Run the tests; expect failure.
- [x] Implement FileManager1 through zbus and portal client/backend through
  ashpd. Keep the optional backend behind a feature and refuse to call itself.
- [x] Send notifications only for background completion/failure and map actions
  back to stable command/job IDs.
- [x] Run maintenance only after first-window readiness. Fetch update metadata
  over HTTPS, verify it with the pinned project key, and offer information only;
  never auto-install an update.
- [x] Run D-Bus introspection checks and sandboxed portal fixtures.
- [x] Commit with `feat(desktop): add D-Bus and portal services`.

### Task 4: Store credentials through Secret Service

**Files:** Create `crates/musheen-desktop/src/secrets.rs`; create
`crates/musheen-desktop/tests/secrets.rs`; modify settings remote models.

- [x] Test locked collection, unavailable service, create/read/update/delete,
  cancellation, renamed connection, settings export, and redacted logs/errors.
- [x] Run the secrets test with a fake service; expect failure.
- [x] Implement credential references keyed by connection ID; settings and
  journals contain references only. When the service is locked or absent,
  offer an explicit session-only secret and never persist it elsewhere. Wipe
  transient secret buffers and exclude credentials from URLs and debug output.
- [x] Run repository scans for fixture credentials and serialized secret fields.
- [x] Commit with `feat(desktop): integrate Linux secret storage`.

### Task 5: Implement operation-scoped privilege

**Files:** Create `crates/musheen-desktop/src/privilege/{mod,broker,polkit,request,rooted_store}.rs`,
`crates/musheen-ui/src/elevated_browser.rs`;
create `crates/musheen-desktop/tests/privilege.rs`; add a small broker binary
under `crates/musheen-desktop/src/bin/musheen-broker.rs`.

- [x] Test authorization denial/expiry, path replacement after approval,
  symlink escape, argument injection, environment scrubbing, allowed operation
  schema, audit record, and broker crash.
- [x] Run the privilege tests; expect failure.
- [x] Implement Polkit authorization for a narrowly typed broker request. Open
  or validate target file descriptors after authorization; allow an optional
  sudo broker transport setting, but never relaunch the GPUI application as root.
- [x] Map “Run as Administrator” and “Open as Administrator” to explicit broker
  operations with a confirmation that names command and target.
- [x] Restrict elevated browsing to the granted local root and show a permanent
  warning banner and privilege icon in every theme. A symlink or breadcrumb
  cannot escape the grant without a new authorization.
- [x] Run the test suite as an unprivileged user with a fake authorizer.
- [x] Commit with `feat(desktop): add scoped administrator actions`.

### Task 6: Build open-in-terminal and the terminal drawer

**Files:** Create `crates/musheen-desktop/src/terminal/{mod,pty,model,profile}.rs`;
create `crates/musheen-ui/src/terminal/{mod,drawer,renderer,input}.rs`;
create tests in both crates.

- [x] Test cwd tracking per active pane, shell/profile selection, resize,
  Unicode wide cells, paste confirmation, OSC title, unsupported escape
  sequences, exit/restart, drawer focus, pane/tab switching, and completeness
  of the default keyboard map including F4.
- [x] Add million-line, long-line, and escape-heavy tests enforcing 10,000
  lines and 64 MiB with oldest-complete-line truncation.
- [x] Implement PTY control with portable-pty, terminal state with
  alacritty_terminal, and GPUI Kit rendering/input. The drawer is resizable,
  closable, keyboard reachable, and never blocks directory rendering.
- [x] Implement external terminal launch through configured desktop entries;
  refuse unrepresentable working directories with a named error.
- [x] Run terminal tests under normal exit, signal, and child-process load.
- [x] Commit with `feat(terminal): add external launch and embedded drawer`.

### Task 7: Close the phase

- [ ] Run fake-service suites with every service absent, slow, disconnected,
  and restarted; the file manager must remain usable.
- [ ] Run format, Clippy, workspace tests, locked release build, license audit,
  D-Bus introspection, accessibility, and visual baselines.
- [ ] Review every process, URI, D-Bus, portal, credential, and privilege boundary
  for injection and path-identity loss.
- [ ] Perform the rule 13 self-review and commit with
  `test: close Linux desktop integration evidence`.
