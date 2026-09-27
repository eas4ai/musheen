# Musheen adversarial audit 2: day five

Date: 2026-09-25 (02:20–03:10 EDT)
Baseline: `main` at `b4b786f` (2026-09-24 23:50, 442 commits). During the review `main` advanced to `988396b` (12 `feat(remote)` commits from `feature/remote-ops`). Every sub-reviewer checked that the files they cite are identical at both revisions unless stated.
Previous audit: `docs/opus-audit.md` (2026-09-22) with its Monitoring log through `7c75bd7` (2026-09-23 09:49).
Scope: read-only. Nothing in the repository was changed except this file and `docs/opus-audit-2/`.
Method: the lead reviewer built and tested an exported copy, ran the real GUI on this desktop, and re-ran the two most serious sub-reviewer claims. Four sub-reviewers read the code in parallel (UI wiring, data safety, desktop and security, spec and process). Their full reports are Appendices A–D.

## Contents

1. Verdict
2. Progress since the first audit
3. What was verified and how
4. Critical and High findings
5. Medium findings
6. Low findings
7. Large folders: measurements
8. Styling status
9. Corrections to the first audit and the Monitoring log
10. Recommended next steps
11. Limits of this audit
12. Monitoring log
Appendix A: UI architecture and command wiring (sub-reviewer A)
Appendix B: operations and local data safety (sub-reviewer B)
Appendix C: desktop, remote, packaging, security (sub-reviewer C)
Appendix D: spec fidelity, parity, process (sub-reviewer D)
Appendix E: commands and raw results

## 1. Verdict

The app is now a working local file manager. Every command reaches a backend. Folders open, selection works with the keyboard and modifier clicks, the file verbs work, dates are local, the font is the desktop font, the sidebar is left-aligned and shows the right drives, and a 200,000-item folder loads in under 10 seconds on a release build with memory flat at about 170 MB. The seven data-safety fixes from the first audit hold, and I could not build a copy or move case that deletes the only complete copy.

It is not ready to ship, for four reasons:

1. **Two common gestures are broken.** Right-click a sidebar place and choose "Open in new tab": the app panics on an `unreachable!()` (confirmed by a test I ran against the exported code). Drag a rubber band after scrolling: it selects the wrong rows, because the scroll handle the fix reads never reports a scroll position for this list type. My Monitoring log marked that bug fixed on 2026-09-23; that verdict was wrong.
2. **Two data-safety gaps remain at the edges.** A cross-device move of a folder that holds one hard-link pair publishes the copy and then stops halfway through deleting the source (confirmed twice by probe). Trash silently falls back to an unverified copy-and-delete across devices when the volume root is not writable (confirmed by the sub-reviewer's probe in a user namespace).
3. **The newest features are delivered but not usable end to end.** Remote profiles show in Network but nothing in the app can store a password. "Extract…" to a chosen folder is always refused. FTPS "Test connection" and FTPS browsing use different TLS modes and ports.
4. **The process did not improve.** The agreed order (data safety → verbs → styling → look at the screen → archive/remote) was not followed: half of the 118 commits since my last review went to release machinery and remote providers, and the "look at the screen" step never happened. No commit body, no screenshot, no benchmark run. The plan checkboxes stopped on 2026-09-22 and no longer describe the work.

The recommendation is the same as on day two: stop new work, fix the four user-facing defects in section 4, put the app on a screen, then continue.

## 2. Progress since the first audit

Status of the 15 next steps from `docs/opus-audit.md` section 9:

| # | Step | Status | Evidence |
|---|---|---|---|
| 1 | Stop the hardening loop | Not done | ~44 release-hardening commits and ~15 archive/remote commits since 09-23 10:00 (Appendix D, Q1) |
| 2 | Fix the startup folder load | Fixed | Wayland frame-loop patch (`bb3c828`); every launch today showed content within 3 s (release) |
| 3 | Opening folders | Mostly fixed | Enter and double-click work everywhere; sidebar context menu "Open" launches an external app and "Open in new tab" panics (4.1) |
| 4 | Selection | Partly fixed | Ctrl-click, Shift-click, Shift+arrows work; rubber band ignores scroll (4.2); in indexed folders the rubber band is a no-op and Ctrl-click after Select All collapses the selection (5.1) |
| 5 | File verbs and shortcuts | Fixed | Delete → Trash, Ctrl+Shift+N, F2, inline rename, system clipboard (`text/uri-list`, GNOME and KDE cut flags) |
| 6 | Paging past 4,096 | Fixed | disk-backed index; 200,000 items verified on screen (section 7) |
| 7 | Readable dates | Fixed | local time with zone |
| 8 | Text and density | Mostly fixed | names at the base size; list rows sized from the theme; secondary cells still `text_sm`/`text_xs`; Grid rows fixed px; tab and breadcrumb still accent pills; dark fallback still light (section 8) |
| 9 | Launch check as a required step | Not done | no screenshot, no GUI note in any of 118 commit messages (Appendix D, Q9) |
| 10 | Data safety 4.1–4.3 | Fixed | hold at HEAD (Appendix B, section 1); two new edge cases (4.3, 4.4) |
| 11 | Broker identity and polkit policy | Fixed in code | unchanged since 09-23; still never run end to end |
| 12 | Terminal paste and volume timeouts | Fixed | unchanged |
| 13 | Connect or remove unused backends | Connected, not usable | archive and remote are wired; "Extract…" refused, no credential entry, SMB refused (4.5) |
| 14 | Split `MusheenApp` | Not done | `app.rs` 18.5k → 27.3k lines, 110 fields, one 12.6k-line `impl` |
| 15 | Decision records for forks | Not done | 1 of 5 forks has a record; ~1,000 patched lines |

Confirmed fixed at runtime today: startup load, local dates, base font size, left-aligned sidebar, drive labels by mount name, the FreeRDP FUSE mount hidden, second-launch forwarding, `ShowItems` selection over D-Bus, 10k/50k/100k/200k folders loading fully and sorted.

## 3. What was verified and how

### 3.1 Lead reviewer checks

Exported copy of `b4b786f` at `scratchpad/rev-b4b786f`, own `CARGO_TARGET_DIR`.

| Check | Result |
|---|---|
| `cargo build --locked --bin musheen` (debug) | ok, 2 m 34 s |
| `cargo test --workspace --locked` | **1,152 passed, 0 failed, 4 ignored** (119 test binaries) |
| The two ignored million-item tests, run with `--ignored` | both pass: `streaming_directory_model_pages_through_one_million_items` 106.5 s, `external_order_supports_one_million_records` 81.2 s |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | clean |
| `cargo build --locked --release --bin musheen` | failed in the shared target dir (`crate zbus required to be available in rlib format`), succeeded in a fresh target dir in 3 m 41 s. Environmental; not a project defect |
| `--all-features` | not built: `libsmbclient-dev` is not installed on this host |

### 3.2 GUI runtime checks

Method: the app ran with private `XDG_*` directories and a copy of `~/.config/kdeglobals`. Windows were moved to the second monitor with a KWin script and captured with `spectacle -f` cropped to the window. The status bar was read with `tesseract`. Runs where the owner's own windows were captured were discarded and the images deleted.

| Check | Result | Screenshot |
|---|---|---|
| Startup on an 18-item folder (debug) | content within 7 s; sizes human; Modified local with `EDT`; a non-UTF-8 name shows `�`; symlink typed "Link" | `opus-audit-2/fixture-folder.png` |
| Startup on the same folder (release) | "18 items" within 3 s, RSS 161 MB | — |
| Sidebar | Home, Places, Storage, Network left-aligned; drives labelled `projects`, `workspace2`, `windows2`, `nvme0n1p2` with free space; no pseudo filesystems; no FreeRDP mount | same |
| Second launch `musheen <folder>` | exits 0 in 0.14 s; the running instance navigates its focused tab (not a new window) | `opus-audit-2/second-launch-forwarded.png` |
| `busctl --user call org.musheen.Musheen … ShowItems` | navigates to the parent and selects the file; status "1 item selected — 7 B" | `opus-audit-2/dbus-showitems-selection.png` |
| `org.musheen.Musheen` bus name | owned 39 of 40 polls over 80 s (missing only at t=0) | — |
| Large folders | see section 7 | `opus-audit-2/200k-*.png` |
| Shutdown on SIGTERM | exits in 0.2 s; the current tab's index directory is left behind (section 5.1) | — |
| Log noise | "could not export org.freedesktop.FileManager1: name already taken" every ~10 s while Dolphin owns the name (`app.rs:2363`) | — |

Not checked at runtime: any mouse gesture (no input-injection tool works on this Wayland session), archive create/extract from the menu, Trash from the menu, the Settings window.

### 3.3 Probes I ran against the exported copy

| Probe | Result |
|---|---|
| Sidebar menu → dispatch `directory.open_new_tab` (headless GPUI test, Appendix E) | **panicked** at `app.rs:9119` `unreachable!("the caller pairs each local command with typed parameters")`; `file.open` on the same target tried to launch an external application |
| Cross-device move (tmpfs → ext4) of `tree/{a, plain, sub/a-link}` where `a-link` is a hard link to `a` (sub-reviewer B's probe, re-run by me) | `Err(SourcePartiallyRemoved)`, `publication_state: Published`, destination complete, **source left with `["sub", "sub/a-link"]`** |

### 3.4 Sub-reviewers

Four agents, read-only, code reading plus small probes on the exported copy. Their reports are verbatim in Appendices A–D. Where I say "confirmed" below, either the sub-reviewer traced the full code path or ran it, or I re-ran it.

## 4. Critical and High findings

### 4.1 Sidebar "Open in new tab" crashes the app (Critical, confirmed by test)

`dispatch_local_target_command` (`app.rs:8971`) only navigates for `Open*` actions when the target is in the origin tab's in-memory item list (`app.rs:8983-9034`). A sidebar place is never in that list unless the current folder is its parent, and in an indexed folder (over 4,096 items) the in-memory list is always empty. The fallback `match` (`app.rs:9093-9119`) has no arm for `OpenInNewTab` or `OpenInOtherPane` and ends in `unreachable!()`. A panic inside a GPUI listener aborts the process.

Reproduced: a headless test that builds the sidebar context menu for a folder outside the current one and calls `dispatch_context_entry` on `directory.open_new_tab` panics at `app.rs:9119`. The same path makes `file.open` on a sidebar folder launch the external `inode/directory` handler instead of navigating, and `OpenInNewWindow` returns "target changed".

No test dispatches these two commands. Fix: resolve the directory the way `activate_directory_item` (`app.rs:6598-6640`) already does (view item → index `lookup_id` → `resolve_item`), handle all four `Open*` actions there, and replace the `unreachable!` arms with an error message.

### 4.2 Rubber-band selection ignores the scroll position (High, confirmed in code)

`render_items` reads `logical_scroll_top()` from the list's `ScrollHandle` (`app.rs:14416`). That method (`vendor/gpui-pre/src/elements/div.rs:4426-4437`) needs `child_bounds`, which only `Div::prepaint` fills. The directory list is a `uniform_list` (`app.rs:14917`, `14944`), and `uniform_list.rs` never writes `child_bounds` (zero hits). So the call always returns `(0, 0 px)`, and `rubber_band_indices` counts rows from the top of the list at any scroll position.

The four `rubber_band_geometry_*` tests feed hand-built scroll values, so they pass. The fix at `8ca028a` was recorded as "D2 fixed" in the Monitoring log; it is not (section 9). Fix: derive the first visible row from `base_handle.offset().y` and the known row height, and add an input-driven test that scrolls first.

### 4.3 Cross-device move of a folder with a hard-link pair leaves a half-deleted source (High, confirmed by probe, twice)

The removal fingerprint (`operation.rs:1196-1215`) includes `ctime` for every non-directory entry. Unlinking one name of a multiply-linked inode changes that inode's ctime, so when removal reaches the second name its identity no longer matches and removal stops with `SourceChanged` after some entries are gone (`SourcePartiallyRemoved`). The destination is complete and verified, so no data is lost, but the user is left with a half-deleted source and a NeedsAttention entry that does not say the copy is complete. Retry re-copies the remainder and then fails with a destination conflict.

Any tree with internal hard links triggers it: pnpm `node_modules`, `cp -al` or rsync `--link-dest` snapshots, Steam and Proton prefixes. Fix: drop ctime from the identity when `stx_nlink > 1`, or refresh the expected identities of the other names of the same inode after each unlink. Add a real-filesystem test with a hard-link pair, a sparse file, and a symlink in one tree.

### 4.4 Trash silently copies across devices without verification (High, sub-reviewer probe)

`move_to_trash` (`mutation.rs:1594-1630`) hands the item to the `trash` 5.2.9 crate with no capability check. When `$topdir/.Trash-$uid` cannot be created (volume root not writable, nested btrfs subvolume), the crate falls back to the home trash and, on `EXDEV`, does `std::fs::copy` / `copy_dir_all` then `remove_dir_all`: no verification, no fsync, no metadata. Probe in a user namespace: mtime lost, a hard-link pair broken, a 64 MiB sparse file expanded to 64 MiB, source deleted. If the copy fails part way (ENOSPC on `$HOME` is likely for a large tree) the partial payload stays as an orphan without a receipt; if the source removal fails part way, the source is half-deleted and the complete copy is invisible to the Trash view. This breaks the project's own verify-before-remove rule. Fix: resolve the trash directory the crate will use before calling it, refuse with `TrashUnsupported` when it is on another device, and offer "Delete permanently" instead.

### 4.5 Remote and archive features are delivered but not usable end to end (High)

- **No credential entry.** Saved FTP/FTPS/WebDAV/HTTP/SFTP profiles appear under Network (`providers.rs:117-154`), but nothing in the app can store a password: `settings/window.rs:1154` asserts there is no `remote.credential` input, and `providers/remote.rs:92` only reads existing Secret Service items. Password logins work only if the user seeds the keyring with another tool. Thirteen remote commits landed on top of this gap.
- **"Extract…" is always refused.** `resolve_context_destination` (`app.rs:7828-7843`) accepts only `copy_to` and `move_to`, so `archive.extract` fails after the destination picker while the command-surface matrix and the static capability table list it as supported. "Extract here" and Compress work.
- **FTPS test and browse disagree.** "Test connection" (`remote/probe.rs:283-285`, default port 990) does implicit TLS; browsing goes through OpenDAL, which defaults to port 21 and explicit `AUTH TLS` (`remote/ftp.rs:60-72`). Saving requires a passing test, so users either cannot save a working explicit-FTPS server or save one that cannot browse.
- SMB and NFS are refused (`providers/remote.rs:134-145`); `pavao` is compiled into the Arch package but unreachable.

### 4.6 The agreed order was not followed and the app was never checked on screen (High, process)

Of 118 commits since 2026-09-23 10:00: about 17 went to step 2 (verbs), 31 to step 3 (styling, paging, sidebar), **0 to step 4 (GUI verification)**, 15 to step 5 (archive, remote), and about 44 to release hardening that was never agreed (packaging, Docker matrix, migrations, benchmarks). Step 5 started at 10:01 on 09-23, while inline rename, links, duplicate, hide, and templates (step 2) landed on the evening of 09-24. All 118 commit messages have empty bodies. There are no screenshots and no GUI checks in any script or test outside Docker; the only window-open proof is an Xvfb `xwininfo` in the Arch package build, which shows a window maps, not what is in it. `AGENTS.md` asks for screenshots on UI changes.

### 4.7 Nothing has been measured (High, process)

Eight `perf: measure` commits (09-24 13:21–15:09, ~2,130 lines) added real benchmark programs under `benches/`. They are only run by `scripts/run-benchmarks.sh` inside the release container. The one release build that contained them (09-24 16:02) was cancelled at step 15/17. No numbers are committed; `docs/performance-baseline.md` (required by plan 07) does not exist. `scripts/benchmark-validations.jq` checks counters against the spec limits but sets no threshold on `wall_ns`, `cpu_ns`, or `peak_rss_kib`, and some counters are literals (`"queued_pages_max": 1` in `benches/directory.rs`). `tests/release_runner.rs` runs the script with a fake `cargo` and fake bench binaries.

## 5. Medium findings

### 5.1 UI (Appendix A)

| # | Finding | Where | Fix |
|---|---|---|---|
| A-F3 | Every watch event in an indexed folder rebuilds the whole on-disk order (5–7 JSON passes over all records) under the index mutex; K events cost K rebuilds; a failed merge sets an error the UI never shows | `directory.rs:283-330`, `index.rs:376-395`, `app.rs:14320-14331` | append to a change log and merge incrementally; debounce; surface the error |
| A-F4 | Catalog `flock` + JSON read/write on the UI thread on every watch event (`observe_present/observe_missing`) | `app.rs:4510-4524`, `app/catalog.rs:599-609` | move off-thread or batch |
| A-F5 | Ctrl-click after Select All in a folder over 4,096 items collapses the selection to one item | `app.rs:6201-6250` | give `IndexedSelection` a `remove` |
| A-F6 | Rubber band and Columns layout are silent no-ops in indexed folders (`visible_items()` is empty) | `app.rs:6298-6349`, `5046-5070` | select ranges through the index reader; feed columns from the index |
| A-F7 | `stat`, `statfs`, and `lstat` on the UI thread for every context menu and several commands; Open on 10,000 selected items = 10,000 `lstat` calls | `app.rs:7473-7475`, `7536`, `10265` | move to a background task |
| A-F8 | Theme fallback still applies Adwaita dark then light, so the last (light) wins | `app.rs:2762-2776` | apply the preferred variant last |
| A-F9 | If the first spill to disk fails (no `$TMPDIR`, ENOSPC), the 4,096 items on screen are dropped and paging stops | `directory.rs` `prepare_index_page`/`finish_index_page` | keep the in-memory items on failure |
| Lead | The index lives in `std::env::temp_dir()` (`index.rs:233`), which is tmpfs on this machine (`/tmp` 128 G) and on many distributions, so the "disk-backed" index is RAM-backed. Records are JSON with path bytes as arrays: 66.5 MB for 200,000 entries (332 B each), so 1 M entries is about 330 MB of RAM | `index.rs:233` | use `$XDG_CACHE_HOME`; a compact record encoding |
| Lead | The current tab's index directory is never removed when the app exits on SIGTERM: 10 → 19 `musheen-directory-*` directories over my runs (3.5–69 MB each); Codex's own test runs left eight on 09-24 | `index.rs` `Drop` never runs on `process::exit` | remove on a signal handler or sweep stale directories at startup |
| A-F10 | `directory.share` is in the matrix but has no backend and no dispatch arm | `app.rs:16487`, `10511-10598` | remove or wire |

### 5.2 Data safety (Appendix B)

| # | Finding | Where | Fix |
|---|---|---|---|
| B-M1 | The `RENAME_NOREPLACE` fallback (NFS, sshfs) builds the destination directory in place and, on a mid-way error, leaves a partial tree under the user-visible name and reports `PublishUnknown` (confirmed by probe) | `operation.rs:895-962` | remove the partial destination before returning, or publish through a second staging dir |
| B-M2 | Sparse files inside a moved or copied folder are fully expanded with no warning (64 MiB allocated for 4 KiB of data, empty metadata report) | `operation.rs:536-555`, `copy.rs:660-665` | reuse `sparse_copy` per entry; record `SparseLayout` |
| B-M3 | Restoring a trashed symlink that points to a directory always fails and leaves an empty directory at the original path, which blocks later restores | `trash` crate `freedesktop.rs:396-420`; `mutation.rs:1671-1679` | restore through musheen's own `renameat2(NOREPLACE)` |
| B-M4 | One orphaned `.trashinfo` makes the whole Trash view fail (`list_trash` → `Err(Missing)`), and trash undo disappears | `mutation.rs:229-258` | skip entries whose payload is missing; offer purge |
| B-M5 | Full trash listing on the UI thread once per finished trash job and up to 100 times per second while the status center is open | `operations.rs:1199-1218`, `queue.rs:240-275`, `app.rs:4114-4122` | compute undo candidates on the worker; check availability by one `stat` |
| B-M6 | Scheduler jobs and events, and the persisted status history, grow without bound; the whole model is cloned on the UI thread and rewritten in full on every state change | `scheduler.rs:169`, `507`; `status_center.rs:208-235`; `operations.rs:943-965` | cap retained entries; drop terminal jobs; drain events |

### 5.3 Desktop, remote, packaging, security (Appendix C)

| # | Finding | Where | Fix |
|---|---|---|---|
| C-N2 | Elevated browsing prompts for the admin password on every folder (`auth_admin`, one `pkexec` per request), and a listing over 64 KiB blocks the broker on its pipe until the 120 s timeout (open since `95cc9cf`) | `broker.rs:615-636`, `elevated_browser.rs:364-376`, policy lines 14, 26, 38 | read stdout concurrently; consider `auth_admin_keep` |
| C-N3 | Extraction copies the whole archive into the destination's parent, decodes every entry once to sniff nested archives, then decodes everything again; 7z pays two quadratic passes; a scan over 30 s fails on `max_elapsed` | `extract.rs:96-106`, `618-631`, `172-223`, `889`; `seven_codec.rs:191-192` | sniff by magic bytes; snapshot only when needed; per-entry time limit |
| C-N5 | The SBOM is generated with default features but the Arch package builds `--all-features`, so libarchive and libsmbclient bindings in the shipped binary are missing from `musheen.cdx.json` and the license notices | `scripts/generate-sbom.py:46-78`, `PKGBUILD:31` | pass the release feature set to `cargo metadata`; check locked ⊆ SBOM |
| C-N6 | Four vendored forks (`sevenz-rust2` 591 lines, `gpui-component` 287, `gpui-pre-linux` 81, `gpui-pre` 48) have no decision record, upstream link, or removal condition | `Cargo.toml:157-162`, `docs/decisions/` | one record per fork; upstream the Wayland and menu-a11y fixes |
| C-N7 | Second launch during startup can race D-Bus activation: the bus spawns a third `musheen` with no arguments, which forwards `$HOME`, so the focused tab may end on Home instead of the requested folder (plausible) | `src/main.rs:11-27`, `packaging/org.musheen.Musheen.service` | claim the name before heavy startup, or drop the activation service |
| C-N8 | When built with `portal-backend` and enabled in settings, `org.freedesktop.impl.portal.FileChooser` is callable by any session-bus peer; no `.portal` file is shipped, so nothing legitimate uses it | `portals.rs:557-692`, `app.rs:2184-2199` | ship the `.portal` file and restrict callers, or drop it |
| C-N10 | "Run" spawns the executable by path after the identity check (a small check-to-exec window), runs a `+x` `.desktop` file as a program, and passes the full environment | `launch.rs:116-131`, `app.rs:3933-3941` | exec via `/proc/self/fd`; refuse `application/x-desktop` |
| C-5.7 | `SystemRoots → PinnedSha256` still needs no confirmation; the probe has no header line limit | `connection.rs:104`, `probe.rs` | confirm; bound |

### 5.4 Process (Appendix D)

| # | Finding | Evidence | Fix |
|---|---|---|---|
| D-4 | Plan checkboxes abandoned: 0/41 on release hardening and 0/25 on the index plan against ~62 implementing commits; of 26 sampled ticks, 5 are unproven and 2 were wrong when ticked | `docs/superpowers/plans/*.md` | tick with a commit hash and test name, or delete the boxes |
| D-6 | The Codex-written audit docs cite tests and files that do not exist (`polkit_zbus.rs` deleted in `95cc9cf`; three cross-filesystem tests in `safe-local-operations-audit.md`) and rules the first audit proved false; never revised | `docs/linux-desktop-integration-audit.md`, `docs/safe-local-operations-audit.md` | regenerate from tests or mark historical |
| D-8 | No Docker test run exists for 20 commits on 09-24 10:49–18:52 or for the 13 remote commits after 22:53; host runs leave no record | buildx history | run `check-linux-build.sh` before each push and record it |
| D-9 | The architecture rule "no crate above `musheen-core` may call `std::fs`" was rewritten the same morning the index code needed it (`b0e7c3d`), with no decision record | `docs/superpowers/plans/2026-09-24-*.md` | decision record, or move index I/O behind `musheen-local` |
| Lead | `app.rs` is 27,264 lines with a 12,592-line `impl MusheenApp`, 110 struct fields, nine non-test functions over 200 lines, six globals, and 10 `expect` calls that abort on a compositor refusal to open a window | Appendix A, Q6 | split along the seams listed there before more features land |

## 6. Low findings

- `open::that` still used directly for the info pane's "Open With…" and the release URL (`app.rs:14181`, `15916`).
- `rename_no_replace` has no fallback for filesystems without `RENAME_NOREPLACE`, so rename, hide/unhide, and rename undo fail on NFS and sshfs (`mutation.rs:1374-1381`).
- Cancelling between publish and source removal reports "Cancelled" while both copies exist (`move.rs:132-137`).
- Trash-restore "Replace" still uses an unjournaled hidden backup (`mutation.rs:425-428`).
- Startup recovery and stale-staging sweep run before the first window (`app.rs:2101-2125`).
- A cancelled or denied `pkexec` prompt is reported as "the administrator broker stopped unexpectedly" (`broker.rs:307-317`).
- `FileManager1` validation leaks a thread per request on a hanging mount (`file_manager1.rs:255-284`).
- Clipboard parse rejects `file://localhost/…` while `FileManager1` accepts it; foreign paths are not canonicalized (`clipboard.rs:229-232`).
- Duplicate async stacks (`tokio` + `async-io`/`futures-lite`; `reqwest` + `ureq`) pass the dependency policy; `deny.toml` has no `[bans]`; a private 2-thread tokio runtime is driven by `block_on` from GPUI threads (`opendal_store.rs:742-748`).
- `scripts/check-smb-provider.sh:7` hard-codes `/home/shawn/.config/docker-hub/config`; `ci/linux-build.Dockerfile:2` is an unpinned tag.
- Two of the twenty budget tests are constant mirrors, and the UI's private `MAX_RESIDENT_ITEMS` is never compared with the core constant (`crates/musheen-ui/src/directory.rs:15`).
- `docs/spec/roadmap.md` still says `Current: foundation`.
- The shortcut document is re-parsed on every keystroke (`app.rs:12622-12626`).
- Icons come from a 20-entry extension table, not shared-mime-info (`app.rs:16729-16756`).
- The compositor lists two 1188×800 "normal" toplevels per instance while only one is visible and the session file records one window. Unexplained; seen in every run. Worth a look at `open_window` call sites at startup.
- Log line every ~10 s while another file manager owns `org.freedesktop.FileManager1` (`app.rs:2363`).

## 7. Large folders: measurements

Synthetic folders of empty `eNNNNNN.txt` files; status bar read by OCR; RSS from `/proc`.

| Folder | Build | 5–10 s | 20–30 s | 90–120 s | RSS |
|---|---|---|---|---|---|
| 5,000 (ext4) | debug | — | "5000 items", sorted | — | — |
| 10,000 (tmpfs) | debug | — | "10000 items" at 20 s | — | 241 MB |
| 50,000 (tmpfs) | debug | — | "50000 items" at 30 s | — | 244 MB |
| 50,000 (ext4) | debug | — | "50000 items" at 30 s | — | 248 MB |
| 100,000 (tmpfs) | debug | — | "4608 items loaded — total unknown" at 30 s | "100000 items" at 90 s | 245 MB |
| 200,000 (tmpfs) | debug | "4608 items loaded — total unknown" at 5 s | same at 30–40 s | "200000 items", sorted, at 120 s | 245 MB |
| 200,000 (tmpfs) | **release** | **"200000 items", sorted, at 10 s** | same | — | 170 MB |

What this shows:

- The 4,096-model cap is real: memory stays flat from 10,000 to 200,000 items.
- On a release build a 200,000-item folder is usable within 10 seconds. On a debug build it takes 40–120 seconds, during which the view shows an **unsorted** slice of the first 4,608 entries (arrival order: `e195392.txt` first) with "total unknown" and no progress indication. The count and order only update when the index finishes (`order_rebuilt` only at first spill or completion, `directory.rs:355-357`). Even at release speed a slow disk or a network mount will show this state for a while; a progress count and a sorted partial order would help.
- The two ignored million-item tests pass when run (section 3.1), so the 1 M claim holds in the headless harness. I did not create a million real files.
- Index storage and the exit leak: section 5.1.

Screenshots: `opus-audit-2/200k-loading-40s.png` (debug, mid-load), `opus-audit-2/200k-complete-120s.png` (debug, complete), `opus-audit-2/200k-release-10s.png` (release, complete).

## 8. Styling status

Against `docs/opus-audit.md` section 3.7 and `docs/spec/ui.md`:

| Item | Status | Evidence |
|---|---|---|
| File names at the base font size | Fixed | screenshot; `text_xs` on names removed |
| Row height from the theme font | Fixed for List/Details/Columns (`geometry::control_height`); Grid/Cards rows still `h(px(108.))`, toolbar `h(px(42.))`/`52.` | `app.rs:14305-14311`, `15265`, `12438`, `12476` |
| Secondary text | Still small: Size/Kind/Modified cells `text_sm`, List size `text_xs`, info pane name `text_sm` (15 `text_xs`, 16 `text_sm` in non-test `app.rs`) | `app.rs:15314`, `15366-15375` |
| Centered sidebar | Fixed (left-aligned rows and headers) | screenshot |
| Pill buttons | Mostly fixed: 0 `rounded_full`, 3 dialog `.primary()`; the active tab and the current breadcrumb segment are still accent-filled pills, and toolbar toggles use accent fills | screenshot |
| Unlabeled toolbar icons | Unchanged: two rows of icons, some alike | screenshot |
| Dark fallback | Still light (5.1, A-F8) | `app.rs:2762-2776` |
| Generic icons | Slightly improved (20-extension table) | `app.rs:16729-16756` |

## 9. Corrections to the first audit and the Monitoring log

- **`8ca028a` "D2 fixed" was wrong.** I judged from the diff that reading the scroll handle fixed the scroll offset. The handle never reports a scroll position for a `uniform_list` (4.2). I should have run a scrolled selection or read the vendored handle.
- **`e9698e0` "opened a window on the 5,000-file folder".** The forwarded launch navigates the focused tab of the running instance; it does not open a new window. The session file after today's run shows one window whose history is the three forwarded folders.
- **`0bc901b` duplicate "data" labels.** Resolved as not a defect on 09-23, and `6ceca1a` now labels drives by mount name.
- **First audit A.8** listed the `unreachable!` in `dispatch_local_target_command` as plausible. It is reachable from the sidebar menu (4.1).
- **Monitoring log `e6037eb`** predicted one dialog per item on a FAT move. Not checked today.

## 10. Recommended next steps

1. **Fix 4.1 today.** Resolve non-resident directory targets in `dispatch_local_target_command`, handle all four `Open*` actions, delete the `unreachable!` arms, and add a test that dispatches `directory.open_new_tab` from the sidebar menu.
2. **Fix 4.2.** Compute the first visible row from the scroll offset and row height; add an input-driven test that scrolls before dragging.
3. **Fix 4.3.** Drop ctime from the removal identity for multiply-linked files; add a real-filesystem test with a hard-link pair, a sparse file, and a symlink in one tree; fix B-M2 (sparse) in the same pass.
4. **Fix 4.4.** Refuse Trash when the crate would copy across devices; offer permanent delete. Fix B-M3 and B-M4 while in the trash code.
5. **Then look at the screen.** One screenshot per `feat(ui)`/`fix(ui)` commit, taken on this desktop, attached in the commit body. Compare against Dolphin side by side for the remaining pills, toolbar icons, and secondary text sizes.
6. **Make remote usable or park it.** Add a credential field that writes to Secret Service, make the FTPS probe use the same client as browsing, and fix "Extract…". Until then, hide SMB/NFS and password-only profiles from Network.
7. **Indexed folders.** Show a sorted partial order and a progress count during the index build; move the index under `$XDG_CACHE_HOME`; sweep stale `musheen-directory-*` at startup; merge watch events incrementally; keep the resident items when the first spill fails.
8. **UI thread.** Move catalog `observe_*`, context-menu `stat`/`statfs`, and trash listings off the UI thread.
9. **Run the benchmarks once** in the release container, commit the numbers with the image digest, and add wall-time and RSS ceilings. Until then, stop adding `perf: measure` code.
10. **Plans and docs.** Either tick checkboxes with a commit hash and test name or delete them. Mark the two Codex-written audit docs as historical. Write the four missing fork decision records and the one for `b0e7c3d`.
11. **Split `app.rs`** along the seams in Appendix A, Q6, before more features land on it.
12. **Elevated browsing.** Read the broker's stdout concurrently and consider `auth_admin_keep`; run one elevated action end to end on this machine and record it.

## 11. Limits of this audit

- No mouse or keyboard input could be injected into the Wayland session, so gestures (rubber band, drag and drop, context menus, dialogs) were checked in code and in headless tests, not on screen.
- The 200,000-item ext4 run was not captured (the window was not where the capture expected it; the images showed the owner's terminals and were deleted).
- `--all-features` was not built (`libsmbclient-dev` missing), so the SMB path, the libarchive worker, and the portal backend were reviewed by reading only.
- No elevated action, remote server, or Docker build was run.
- The 12 remote commits that landed during the review (`b4b786f..988396b`) were only skimmed (Appendix C, section 5).
- Sub-reviewer findings marked "plausible" were not executed.
- The owner's own desktop use during the review caused several captures of the wrong window; those runs were discarded.

---

## Appendix A: UI architecture and command wiring (sub-reviewer A)

**Repo state.** The working tree HEAD is `988396b`, 12 commits past the `b4b786f` named in the brief. All 12 touch only `crates/musheen-ui/src/providers*` and remote code; `app.rs` differs from `b4b786f` by one test helper (line 20426+). Non-test line numbers below are identical at both revisions.

**Method.** Read-only code reading in `/home/shawn/workspace2/musheen`. I did not build or run anything: the shared target dir had no `musheen_ui` artifacts and 26 rustc/cargo processes were already running. "Confirmed" below means I traced the full code path from input to effect, not that I executed it. Where a runtime check would be cheap, I say how.

---

### Findings (by severity)

#### F1. Critical, confirmed by code path — "Open in new tab" / "Open in other pane" hits `unreachable!` when the target is not in the resident view

Scenario A (common): right-click a place in the sidebar (Documents, a pinned folder, a mount) → "Open in new tab".
Scenario B: right-click any folder row in a directory with more than 4,096 entries → "Open in new tab" or "Open in other pane".

Path:
1. `sidebar_entry_context_menu` (`app.rs:7048-7069`) builds one `CommandTargetRef` for the sidebar folder; `context_for_menu_with_item` maps `SidebarLocation` + one selection to `CommandTarget::Directory` (`app.rs:7488-7492`). `menus/builder.rs:546` shows `directory.open_new_tab` for `Directory | Mount`; `tests/context_menus.rs:1529-1534` confirms the sidebar menu contains it.
2. Click → `dispatch_context_entry` (`app.rs:7781`) → `pending_with_parameters` (`menus/mod.rs:310-313`) uses the contract from `command.rs:672` (default `Targets(ExactlyOne)`) → `CommandParameters::Targets([dir])`.
3. `dispatch_typed_context_command` arm at `app.rs:8577-8590` → `dispatch_local_target_command` (`app.rs:8971`).
4. The navigation block (`app.rs:8983-9034`) runs only if `directory.view().item(targets[0].id())` finds the target in the origin tab's **in-memory** items. A sidebar folder is not there unless the current folder is its parent. In an indexed directory `view.items` is always empty (`take_items_for_index`, `views/mod.rs`; nothing refills it).
5. `revalidate_context_targets` (`app.rs:10241-10280`) passes (lstat finds the folder).
6. `match (action, parameters)` (`app.rs:9093-9119`) has no arm for `(OpenInNewTab, Targets)` or `(OpenInOtherPane, Targets)` → `unreachable!("the caller pairs each local command with typed parameters")` at `app.rs:9119` → panic inside a GPUI listener → process abort.

Same block, related defects:
- `Open` on such a target reaches `execute_default_application` (`app.rs:9094`) and launches the external `inode/directory` handler instead of navigating (3.3 is back for sidebar and big-folder context menus; Enter and double-click are fine, see F-clean).
- `OpenInNewWindow` on such a target returns the "target changed" message (`app.rs:9111-9117`), so it never works from the sidebar.

Tests: zero tests dispatch `directory.open_new_tab` or `open_other_pane` (0 hits in `app.rs` tests; `tests/context_menus.rs` checks composition only). The sidebar test at `app.rs:23636` checks only `directory.properties`. The previous audit listed this `unreachable!` as "plausible" (A.8); it is reachable.

Fix: resolve the directory target the way `activate_directory_item` (`app.rs:6598-6640`) already does (view item, else index `lookup_id`, else `resolve_item`), handle all four Open* actions in that block, and replace the `unreachable!` arms with an error message.

#### F2. High, confirmed by code path — rubber-band selection ignores the scroll offset (the monitoring log's "D2 fixed" verdict is wrong)

`render_items` reads `rubber_band_scroll.0.borrow().base_handle.logical_scroll_top()` (`app.rs:14392-14395`) for the first visible row and its pixel offset. `ScrollHandle::logical_scroll_top` (`vendor/gpui-pre/src/elements/div.rs:4426-4437`) and `top_item` (`:4294-4310`) read `state.child_bounds`. That vector is written only in `Div::prepaint` from `child_layout_ids` (`div.rs:1999-2006`). `UniformList::request_layout` passes `None` children (`uniform_list.rs:284-317`) and `uniform_list.rs` never writes `child_bounds`. So for the directory list the call always returns `(0, px(0.))`; upstream marks its own `logical_scroll_top_index` as test-only (`uniform_list.rs:221-228`).

Effect: `rubber_band_indices` (`app.rs:461-565`) gets `first_item_index = 0`, `first_item_offset = 0` at every scroll position. After scrolling down, a drag selects rows counted from the top of the list, not the rows under the pointer.

The four `rubber_band_geometry_*` tests (`app.rs` tests, around 17211-17320) feed hand-built `RubberBandSurface` values, so they test the arithmetic, not its inputs. The input-driven tests from `22cc9c5` do not scroll.

Fix: compute `(-base_handle.offset().y / row_height)` floor and remainder (row height is already known), or use `last_item_size`; add an input-driven test that calls `scroll_to_item` first. A one-line debug print of `logical_scroll_top()` after a scroll would confirm this in minutes.

#### F3. High, confirmed by code (not measured) — every watch event in an indexed directory rebuilds the whole on-disk order

`DirectoryIndexWatchWork::run` (`directory.rs:283-330`) appends one record and then calls `index.rebuild_order` (`directory/index.rs:376-395`): identity build over all records with sort runs and merge levels, a dedup pass, then the visible build. That is roughly 5-7 full JSON-decode passes over N records per event, under the index mutex. Viewport reads (`request_indexed_range`, `app.rs:15069`) queue behind it, so the list shows "Loading…" placeholders for the duration. Events are processed one at a time (`continue_watch` runs only after the work finishes, `app.rs:4547-4562`), so K events cost K rebuilds; if the watcher's queue overflows, the code falls back to `Invalidated` → full reload (`app.rs:4569`), which is self-healing but disruptive. The design doc says changes are "merged into a replacement order"; the code does a full rebuild.

Also: a failed merge (for example ENOSPC in `$TMPDIR`) sets `DirectoryState::Error` (`directory.rs:832`), but the indexed watch path never copies it to `operation_error`, and `render_items` shows the error surface only when `item_count == 0` or in Columns layout (`app.rs:14320-14331`). The user sees a silently stale list.

Fix: append to a change log and merge incrementally; coalesce events with a short debounce; surface the error.

#### F4. Medium-High, confirmed — catalog file I/O still runs on the UI thread on every watch event

`observe_watch_event` (`app.rs:4510-4524`) calls `catalog_binding.observe_present/observe_missing` (`app/catalog.rs:599-609`) → `update_result` → `CatalogStore::update`, which takes an exclusive `flock`, reads and parses the JSON catalog, and may rewrite it (`musheen-desktop/src/catalog/mod.rs`, `lock_exclusive`/`load_unlocked`/`save_unlocked`). It is called inside `this.update(...)` for every Created/Changed/Renamed/Removed event on both the in-memory (`app.rs:4505`) and indexed (`app.rs:4557`) paths. `reconcile_directory` was moved to `background_spawn` (`app.rs:4750-4760`); this part was not. A second process holding the lock still freezes the UI. Fix: move it off-thread or batch it.

#### F5. Medium, confirmed — Ctrl+click after Select All in a big folder collapses the selection to one item

`select_item_with_mode` (`app.rs:6201-6250`): for any mode other than Add it calls `directory.clear_indexed_selection()` (`:6235`) and then toggles the id on the now-empty in-memory list, which adds it. In a folder under 4,096 items the same gesture deselects one item; above it, the user ends up with exactly one item selected and no warning. Fix: give `IndexedSelection` a `remove` and flip the arrival bit for Toggle.

#### F6. Medium, confirmed — rubber band and Columns layout are silent no-ops in indexed directories

`update_rubber_band` (`app.rs:6298-6349`) maps indices through `filtered_items` → `view.visible_items()` (`app.rs:5206-5217`), which is empty once the directory is indexed, so nothing gets selected while the band still paints. `navigate` (`app.rs:5046-5070`) decides `descending` from `visible_items()`, so `ColumnTrail::navigate` (`views/columns.rs`) receives no items, clears, and the parent columns disappear. Fix: translate rubber-band indices to a bitmap through `reader.select_visible_range` (async, as Shift+click already does) and feed column panes from the index.

#### F7. Medium, confirmed — blocking metadata calls on the UI thread on every context menu and several commands

`context_for_menu_with_item`: `store.executable_state` (stat, `app.rs:7473-7475`) and `store.capabilities` → `probe::capabilities` → `statfs` (`app.rs:7536`; `musheen-local/src/probe.rs:57,102`). `sidebar_entry_context_menu` `resolve_item` when no identity is cached (`app.rs:7058`). `revalidate_context_targets` lstat per target (`app.rs:10265`; Open on a 10,000-item selection = 10,000 lstat calls on the UI thread). Also `app.rs:9496, 9566, 10098, 10316, 5574`. On a hung NFS or FUSE mount, a right-click freezes the window. This is the previous A.2/B.8 item; still open.

#### F8. Medium, confirmed — theme fallback still ends on light

`install_native_theme` (`app.rs:2762-2776`) applies Adwaita dark and then Adwaita light. `apply` → `apply_inner` (`vendor/native-theme-gpui/src/lib.rs`) assigns `*GpuiTheme::global_mut(cx) = theme` on every call, so the last (light) wins and `last_is_dark = false`. Nothing in the app or vendored crates calls `Theme::sync_system_appearance` (grep finds the definition only), and `install_observer_once` observes the theme global, not the window appearance. This is 3.7's "fallback is always light", unchanged. Fix: apply the non-preferred variant first and the preferred one (from `cx.window_appearance()` or the portal color scheme) last.

#### F9. Medium, confirmed — the first spill to disk drops the 4,096 resident items if index creation fails; index location

`prepare_index_page` takes `view.take_items_for_index()` (`directory.rs:~535`) before `DiskDirectoryIndex::new()` runs inside the worker. If `new()` or the first append fails (no `$TMPDIR`, ENOSPC), `finish_index_page` sets `DirectoryState::Error` and clears `next_request` (`directory.rs:~560`); the items that were on screen are gone. The design says the last valid order stays available "where safe"; here it was safe.
The index lives in `std::env::temp_dir()` (`index.rs:225`, `Builder::new().tempdir()`), which is tmpfs on many distributions, so the "disk-backed" index is RAM-backed there: JSON records plus merge runs for one million entries are hundreds of MB. Consider `$XDG_CACHE_HOME`. (Location confirmed; the tmpfs effect is plausible per distribution.)

#### F10. Low-Medium, confirmed — `directory.share` is in the matrix but has no backend and no dispatch arm

`CommandAction::Share` is in `is_contextual_command` (`app.rs:16487`) but missing from `backend_action_state` (`app.rs:10511-10598`) and from `dispatch_typed_context_command` (falls to `context.backend-unavailable`, `app.rs:8657`). No provider reports `ProviderAction::Share` as Supported (only a core test does), so today it is dead rather than misleading. Remove it from the matrix or wire it.

#### F11. Low, confirmed — the shortcut document is re-parsed on every keystroke

`route_custom_shortcut` (`app.rs:12622-12626`) calls `shortcuts_from_document(&settings.0)` and `all_bindings_for_input` per key press. Cache the parsed map keyed by the settings revision.

#### F12. Low, confirmed — remaining style deviations from `docs/spec/ui.md` / `AGENTS.md`

- Text: file names now use the base size (fixed). Details cells Size/Kind/Modified use `text_sm` (`app.rs:15366-15375`), List size uses `text_xs` (`:15314`), the info pane's item name uses `text_sm` (`:14016, 14040, 14077`). Counts in non-test `app.rs`: 15 `text_xs`, 16 `text_sm` (were 18/21). Dolphin and Nautilus use one size per row.
- Row height: List/Details/Columns rows use `geometry::control_height(list.row_height, item_font, border, native)` (`app.rs:14305-14311`; `vendor/native-theme-gpui/src/geometry.rs:52-61`) = max(theme row height, ceil(font × scale × line height) + 2 × padding). So `96775f5`'s "honor native density" holds for list rows: theme-declared, growing with font scale. Grid/Cards are still fixed: `h(px(108.))`, row 116, widths 128/240 (`app.rs:15265, 14968, 561-565`), which clips at 200% scale (UIV-024). Toolbar `h(px(42.))`/`52.` (`:12438, 12476`) and the 32 px details header are fixed too.
- Pills and alignment: 0 `rounded_full`; only 3 `.primary()` buttons (dialog confirm, Save, Try again). Sidebar rows are ghost/small buttons with a left-aligned `w_full` flex child (`app.rs:12866-12890`), so the centered-sidebar problem appears fixed in code (not verified visually).

#### F13. Low, confirmed — icons come from a 20-entry extension table

`content_identity_for_item` (`app.rs:16729-16756`) maps a fixed list of extensions to MIME types; no shared-mime-info lookup. `MimeDetector` exists but is used only for Open With.

---

### Q1: does every command reach a backend?

All 84 `CommandAction` variants except `Share` (F10) have a dispatch arm in `dispatch_typed_context_command` (`app.rs:8339-8660`) or `dispatch_action` (`:5757-5830`), and `backend_action_state` now lists every verb (3.2 fixed). Key routes:

- Static chords (`toolbar.rs:12-38`): Alt+Left/Right/Up, F5, Ctrl+L, Ctrl+F, Ctrl+Shift+F, Ctrl+Shift+P, Ctrl+T/W/Shift+T, F3, F6, Ctrl+A, Ctrl+H, Ctrl+1..6, Ctrl+B, Alt+Enter; plus Enter/Up/Down/Shift+Up/Down/Escape/Shift+F10/Menu/F4 (`app.rs:97-115`).
- Configurable registry defaults (`command.rs:1725-1915`): Ctrl+X, Ctrl+C, Ctrl+V, F2, **Delete → `file.move_to_trash`**, Shift+Delete → `file.delete_permanently`, **Ctrl+Shift+N → `create.directory`**, Ctrl+, → settings. They reach `route_custom_shortcut` → `dispatch_command` (`app.rs:5323`), which remaps Delete to permanent delete inside Trash (`:5330-5334`) and routes through the same registry projection as a menu row.
- Open/Enter: `ActivateDirectoryItem` → `activate_directory_item` (`app.rs:6598-6640`) handles resident and indexed items and navigates for folders. Double-click: `app.rs:15420-15427`. Context-menu Open on non-resident targets: F1.
- Copy/Cut (`app.rs:8349-8380`): in-app `FileClipboard` plus `SystemFileClipboard::publish`. Formats written (`musheen-desktop/src/clipboard.rs:9-11, 120-160`): `text/uri-list` (CRLF), `x-special/gnome-copied-files` (`copy`/`cut` + LF URIs), `application/x-kde-cutselection` (`0`/`1`).
- Paste (`app.rs:10753-10790`): `system.read()` and per-path lstat run in `background_spawn`. `read()` accepts any owner offering `text/uri-list` or the GNOME format; `parse` decides cut/copy from the GNOME first line, else KDE `1`, else Copy — so an external plain `text/uri-list` pastes as Copy. Cut is honored from GNOME and KDE writers. Publishing on copy runs on the UI thread (X11 selection-owner round trip; low).
- Rename: inline (`begin_inline_rename`, `app.rs:9213`) with dialog fallback; undo for trash/rename/move is through the status-center Undo button → `operation_hub.submit_undo` (`app.rs:15645`, `operations.rs:713`). There is no Ctrl+Z and no `edit.undo` command.
- Duplicate `:9621`, Hide/Unhide `:9552`, symbolic/hard link → `NameOperation::Link` dialog, templates → portal file picker `:9660` (requires the origin tab to still be focused at the same location, else "target changed"), NewDirectory/NewEmptyFile → name dialog, CopyLocation → `cx.write_to_clipboard` `:8390-8409`, MoveToTrash/DeletePermanently → `submit_delete_targets` `:10829`, Compress/Extract/ExtractHere → `build_archive_plan` + `operation_hub.submit_archive` `:8674-8695`, BrowseArchive → new window with `ArchiveStore` and `NoArchivePasswords` (`:8696-8770`; encrypted archives cannot be opened), tags/pin/mount/privilege/custom actions all have arms.
- Column navigation: works for in-memory folders; empty in indexed folders (F6).

Nothing is stubbed behind an "unsupported" state any more; the failures are in target resolution (F1) and indexed-mode gaps (F5, F6).

### Q2: disk-backed index

- **Where/how:** `$TMPDIR/musheen-directory-XXXX/` (mode 0700) with `records` (`MSIDX001` + `[u32 len][JSON record]`…) and `offsets` (u64 LE per arrival), plus sorted-run temp files (`index.rs:14-16, 225-245`). Records are keyed by arrival ordinal; identity is `ItemId` (inode-based locally). `rebuild_order` builds an identity order (id asc, arrival desc), dedups to the latest record per id, drops tombstones, then builds the visible order with the view comparators plus arrival as tie-breaker (`index.rs:376-395, 397-425, 700-712`).
- **Write failure:** `append_record` rolls back both files with `set_len` (`index.rs:263-303`); tests cover `/dev/full`, a read-only file, and a truncated record. Page failure → `DirectoryState::Error`, paging stops (F9 for the first spill). Watch failure → silent (F3).
- **Sort/filter/watch and targets:** preference and filter changes call `schedule_index_order` (`app.rs:5944, 6191, 11981, 12009, 14866`) with an `order_token` revision guard; the resident viewport is dropped on completion. Every async completion checks `(generation, order_epoch, indexed_selection_epoch, selected_ids)` before applying (`app.rs:5375-5383, 6100-6108, 6820-6830, 15082-15095`), so stale targets are discarded. Selection is an arrival bitset carried forward on Changed/Renamed and dropped on Removed (`directory.rs:806-816`).
- **Retention:** `IndexedViewport` holds at most 16 ranges / 4,096 rows (`app.rs:374-421`); the in-memory model clamps to 4,096 with pinned-item eviction (`views/mod.rs:trim_unpinned`). Real.
- **Fallback:** `needs_index` spills only when `len + page > 4,096` (`directory.rs:~520`); 4,096 items stay in memory, 4,097 spill.
- **Paging trigger:** pages chain automatically (`finish_directory_page`, `app.rs:4782-4789`; after reconcile in indexed mode) and on scroll wheel (`:14497`), so the whole directory is always enumerated. During a long load the visible count stays at the first-spill count until the last page (`order_rebuilt` only at first spill or completion, `directory.rs:355-357`), then jumps.
- **Off-by-one / identity confusion / races:** I checked `read_range` bounds, inclusive range ends in `select_between_ids` and `move_focus`, `position_of_id` binary search, `carry_forward` growth, and tombstone dedup; no off-by-one found. Watch events are serialized per tab, and page and watch work share one `Arc<Mutex>`, so ordering is preserved. The UI thread never locks the index (all readers are in `background_spawn`). The identity gap is F5 (Toggle clears the bitmap).

### Q3: selection

In-memory `SelectionModel` is a `Vec<ItemId>` with O(n) `contains`/toggle (`views/selection.rs`), so Select All on 4,096 items is O(n²) ItemId compares and each rendered row does `selected_ids().contains()` (`app.rs:15165`); bounded, but the "bounded bitset" work applies only to the indexed path. Shift+click and Shift+Up/Down use on-screen order (`select_to_item_in_order` with `filtered_items`), correct under sort and filter. Rubber band: F2 (scroll), F6 (indexed). Row-height assumptions in the rubber band (116/108 grid, native list height) match the rendered sizes (`app.rs:14968, 15265`).

### Q4: thread model

Off the UI thread: directory pages, watcher polling, all index work and reads, search, info pane, thumbnails, clipboard read plus lstat, `reconcile_directory`, session save (`prepare_save` on UI, `save_if_current` spawned), status persistence, properties loading, terminal and executable launch. Still on the UI thread: F4 (catalog `observe_*` per watch event), F7 (stat/statfs/lstat in menus, revalidation, hide/unhide, properties), `catalog_binding.update` for pin and tag edits (`app.rs:5128, 5990, 10210-10215`; user-initiated), clipboard publish on copy, `DirectoryViewModel::extend` rebuilding a HashMap of all items per watch event (`views/mod.rs:281-297`), `filtered_items` allocation per mouse move during a rubber band (`app.rs:6318-6322`), F11. Mutex on the render path: `operation_hub.status().lock()` in `operation_status_summary`/`operation_status_entries` (`app.rs:15532, 15585`), a std Mutex shared with operation workers (short holds). `volumes.snapshot()` clones a cached model under an RwLock (`runtime.rs:261`), no I/O.

### Q6: structure

- `app.rs`: 27,264 lines; non-test 16,831; `impl MusheenApp` spans 3370-15961 (12,592 lines); test module 10,433 lines with 146 tests (109 `#[gpui_kit::test]`, 37 `#[test]`).
- `MusheenApp`: 110 fields (was about 95).
- Non-test functions over 200 lines: `dispatch_typed_context_command` 335 (8339), `context_for_menu_with_item` 279 (7434), `render_item` 265 (15211), `render` 258 (15963), `render_sidebar` 247 (12745), `render_items` 247 (14257), `new_with_navigation` 246 (3843), `render_trash_surface` 222 (13184), `render_terminal_drawer` 214 (3553).
- Globals: `FileManagerWindows`, `DesktopMaintenanceOwner`, `DesktopNotificationOwner`, `DesktopPortalClient`, `RuntimeSettings`, `RemoteConnectionsRevision` (`set_global` at 2069-2070, 2188, 2323, 2474, 2530) and `static CLIPBOARD: OnceLock` (`app.rs:715`).
- Caches: `icon_cache` is unbounded but keyed by icon name (tens of entries). Per-tab maps `directories`, `indexed_viewports`, `directory_scrolls`, `column_*` are pruned on tab close with `cancel()` (`finish_navigation_change`) — clean. `info_panes`, `sidebars`, `trash_focus`, `searches`, `filters`, `trash_states` are not pruned there (slow leak per closed tab; low).
- `unwrap`/`expect`: non-test `app.rs` has 0 `.unwrap()`, 213 `.expect(`, 11 `unreachable!`/`panic!` (the 1,369 figure counts tests). Risky: `unreachable!` at 9119 (F1); 10 window-open `.expect` calls (5644, 5681, 5748, 8086, 8226, 9436, 9789, 11121, 11774, 13524) abort on compositor refusal (plausible, low-medium); the rest are locale lookups, lock poisoning, and invariants. Test hooks `command_dispatch_probe`/`trash_purge_probe` are `#[cfg(test)]` (`app.rs:3307-3310`), compiled out.
- Split shape: (1) `directory_controller` (load/page/watch/index glue, 4233-4830 and 14905-15140), (2) `selection` (6201-6600), (3) `command_dispatch` (5323-5870, 7781-9200), (4) `context_menus` (6667-7780), (5) `render/*` (12000-16300), (6) `clipboard` (700-760, 8349-8380, 10753-10830), (7) `windows` (properties, dialogs). Each already owns a distinct subset of the 110 fields.

### Q7: tests

- Counts: 146 in `app.rs`; 184 in `tests/*.rs` (11 gpui, 173 sync).
- Gated: two `#[ignore]` million-item tests (`directory/index.rs:1198`, `tests/shell.rs:230`), which are the design doc's headline verification. `portal-backend` (default off) gates the portal client (`app.rs:2196-2302, 8098`), so default runs never exercise it.
- Tautological-leaning: three tests set `command_dispatch_probe` (`default_shortcuts_reach_live_handlers_in_browser_and_input_contexts` 19216, `indexed_directory_rows_use_full_count_and_load_viewport` 22278, `trash_delete_key_and_menu_purge_only_confirmed_live_receipts` 24168). `dispatch_command_request` returns before dispatch when the command is enabled (`app.rs:5438-5446`), so they prove "reached and enabled", not the effect; the first test's name overstates. The four `rubber_band_geometry_*` tests feed the scroll inputs by hand (F2).
- Doc guards: `tests/context_menus.rs:1886, 2010` regenerate and compare `docs/command-surface-matrix.md` (a drift check). `tests/settings.rs` round-trips fixture files (real behavior).
- Missing coverage: any dispatch of `directory.open_new_tab`/`open_other_pane` (F1), a scrolled rubber band (F2), Ctrl+click after Select All in indexed mode (F5), watch-merge error surfacing (F3).

### Previous audit items → status at HEAD

| Item | Status | Evidence |
|---|---|---|
| 3.1 startup load stalls | Claimed fixed (07e13dd, bb3c828); not verified here (no GUI run) | `startup_load_started`, synchronous first load |
| 3.2 core verbs not connected | Fixed | `backend_action_state` 10511-10598; arms 8339-8660; default chords listed in Q1 |
| 3.3 cannot open folder from list | Fixed for Enter/double-click; context-menu Open on sidebar or big-folder targets launches an external app, and Open in new tab/other pane panics | F1 |
| 3.4 4,096 cap | Fixed by the disk index; new gaps | F3, F5, F6, F9 |
| 3.5 raw Unix seconds | Fixed | `format_modified`, app.rs:15376 |
| 3.6 block devices in sidebar | Claimed fixed (361eb3b, 0bc901b); not verified | — |
| 3.6 centered sidebar | Appears fixed in code | app.rs:12866-12890 |
| 3.6 generic icons | Partially improved | F13 |
| 3.7 tiny primary text | Fixed for names; secondary cells still small | F12 |
| 3.7 fixed 38 px rows | Fixed for List/Details/Columns; Grid/Cards fixed px | F12 |
| 3.7 pills / accent buttons | Fixed | 0 `rounded_full`, 3 dialog `.primary()` |
| 3.7 fallback always light | Still open | F8 |
| 6 / A.2 catalog I/O on UI thread | Partially fixed (reconcile off-thread); observe_* still on UI thread | F4 |
| A.2 stat per context command | Still open | F7 |
| A.3 re-sort per row per frame | Fixed | cached `visible_order` |
| A.3 watch overflow leaves stale view | Fixed (Invalidated → reload, app.rs:4569); per-event costs remain | F3 |
| A.4 god object | Worse (27k lines, 110 fields, 12.6k-line impl) | Q6 |
| A.6 tests | More tests; 3 probe-only; 2 ignored | Q7 |
| A.8 `dispatch_local_target_command` unreachable | Confirmed reachable | F1 |
| A.8 window-open expects | Still present (10 sites) | Q6 |
| Monitoring D2 rubber band vs scroll (marked fixed at 8ca028a) | Not fixed | F2 |
| Monitoring D3 selection uses screen order | Fixed | `select_to_item_in_order` |
| Monitoring D4 rubber band on chrome/scrollbar | Fixed | `is_rubber_band_start` |
| 16c41f0 "(g) persist_status on UI thread" | Fixed (72c237a) | Q4 |

### Checked and found clean

- Enter and double-click open folders in both in-memory and indexed directories.
- Delete → Trash with review; Shift+Delete requires confirmation; Delete remapped inside Trash; Ctrl+Shift+N → name dialog.
- Clipboard formats, cut/copy detection, external `text/uri-list` acceptance; reads and lstat off-thread.
- Index record round trip with non-UTF-8 paths and provider keys; append rollback; a corrupt new record cannot replace the last valid order; owner-only temp dir removed on drop.
- Stale-generation rejection on every async completion; watch events serialized per tab; index never locked on the UI thread.
- Tab close prunes directory models, viewports, and scroll handles and cancels loads; `Drop` cancels loads, searches, info panes.
- Session save and status persistence off-thread; sort/filter/hidden changes rebuild the indexed order and drop the resident viewport.
- No `todo!`/`unimplemented!`; 0 `.unwrap()` in non-test `app.rs`; test probes compiled out.
- List row height and scrollbar width come from the native theme.

### Judgment

The command surface is now wired end to end, and the indexed-directory design is sound in its core (bounded residency, arrival-keyed selection, generation guards, serialized watch events, no UI-thread index locks). But two of the most common gestures are broken in ways the current tests cannot see: right-click → "Open in new tab" from the sidebar panics the app (F1), and rubber-band selection is wrong as soon as the list is scrolled (F2) — the latter was recorded as fixed. The indexed path is functional for browsing and keyboard use but not for rubber band, Ctrl+click deselect, Columns view, or folders that change under a watcher (F3, F5, F6). The UI thread still does catalog flock/JSON work per watch event and stat/statfs per context menu (F4, F7), and the theme fallback still ignores a dark desktop (F8). Fix F1 and F2 before any wider testing; they are small, local changes with obvious tests. F3 and F4 are the next structural items. The `app.rs` god object has grown by 50 percent since the last audit and should be split along the seams listed in Q6 before more features land on it.

---

## Appendix B: operations and local data safety (sub-reviewer B)

**Method.** Read-only code review of `musheen-ops`, `musheen-local`, `musheen-ui/src/{operations,status_center}.rs`, `musheen-desktop` persistence, `musheen-core` limits/cancel, `src/instance.rs`, plus the vendored `trash 5.2.9` crate that the local provider delegates to. Seven probe tests were appended to the exported copy only (`scratchpad/rev-b4b786f`, since restored to pristine; probe sources kept at `scratchpad/opus-b-probe/*.with-probes`) and run against `/dev/shm` (tmpfs) ↔ scratchpad (ext4), and inside an unprivileged user+mount namespace for the trash probe. Note: the repo `main` is now at `988396b` (12 remote-provider commits after `b4b786f`); all line numbers below are for `b4b786f`, and the files I cite in `musheen-local`/`musheen-ops` are byte-identical between the two (checked with `cmp`).

### 1. Status of previous items at `b4b786f`

| Item | Status | Proof |
|---|---|---|
| 4.1/B.1 Replace + partial removal deletes only copy | **Fixed** | `copy.rs:463-471` `after_partial_source_removal`; `mutation.rs:1171-1181` refuses rollback unless `destination_can_be_removed_for_rollback()`; real-FS test `mutation.rs:2188` |
| 4.2/B.2 atime breaks cross-device move | **Fixed** | `operation.rs:297-309` identity = dev, ino, size, mtime, ctime (no atime); `verify.rs:4-9`; test `operation.rs:1572` |
| 4.3/B.3 job IDs restart at 1 | **Fixed** | `operations.rs:325-328` → `LocalOperationQueue::starting_after(highest_job_id)`; `scheduler.rs:134-144` |
| 4.4/B.4 unjournaled Replace backup / stale staging | **Fixed** | `ReplacementTransaction` `mutation.rs:779-867`, recovery `892-1018`, merging marker `1281-1289`; startup cleanup `mutation.rs:150-187` + `operations.rs:1376-1409`. Trash-restore Replace still uses an unjournaled `.musheen-restore-backup-<pid>-<n>` (`mutation.rs:425-428`), see Low L3 |
| 4.5/B.5 no `RENAME_NOREPLACE` fallback | **Partial** | `operation.rs:247` and `866-868` fall back on `EINVAL`/`ENOSYS`; but directory publish is non-atomic and leaves a partial destination on failure (**confirmed by probe P3**, finding M1); `rename_no_replace` at `mutation.rs:1374-1381` has no fallback at all, so rename/hide/unhide/undo-rename fail on NFS and sshfs |
| 4.6/B.6 metadata loss unreported | **Mostly fixed** | `move.rs:140-154` returns `MoveMetadataReview`; `queue.rs:1308-1319`. Gap: `SparseLayout` is only noted for a root regular file (`copy.rs:660-665`); sparse files inside a directory are silently expanded (**probe P4b**, finding M2) |
| 4.7/B.7 removal token after publish | **Fixed, but over-strict** | token taken before verify `copy.rs:574-585`; `operation.rs:278-289`. The token includes ctime per file (`operation.rs:1207-1213`), which breaks every tree with an internal hard-link pair (**probe P4a**, finding H1) |
| 4.8/B.8 FS work on UI thread during drag hover | **Not re-verified** | the path still exists: `operations.rs:433-437` `can_accept_drop` → `queue.rs:1594-1711` `inspect_drop` → `writable_directory` `1851-1867` → `probe.rs:53-76` (statfs + full mount-table parse) under the queue mutex |
| 4.9/B.9 ops crate not connected | **Mostly fixed** | trash `app.rs:10876`, rename `9480/9584`, links `9508/9520`, duplicate `9627`, permanent delete `10872`, all through `queue.rs:96-132`. Plain copy/move still write no `Journal` (the type is used only by `musheen-desktop/src/archive`); they are safe by construction (staging + startup cleanup) |
| B.10 recursive target by path | **Fixed** | `queue.rs:1610`, `1646-1651` canonicalize both sides |
| B.10 FIFO/socket fails whole folder copy | Unchanged (by design) | `operation.rs:490-494` |
| B.10 double re-hash in verify | Unchanged | `operation.rs:189-194` |
| B.10 hard links across top-level items expanded | Unchanged | one `CopySession::default()` per job, `queue.rs:711`, `717` |
| B.10 non-UTF-8 names | Still clean | bytes end to end (`visibility_rename_name`, `duplicate_name`, staging parsing) |
| B.11 judgment items | see Judgment | |

### 2. New findings

#### H1 — High, **confirmed by probe**: cross-device move of a directory that contains a hard-link pair publishes the copy, then deletes only part of the source and stops

- **Where:** `operation.rs:1196-1215` (`descriptor_identity` includes `stx_ctime` for non-directories), `1098-1141` (`execute_descriptor_removal` revalidates each entry before `unlinkat`), `1031-1055` (`SourcePartiallyRemoved` once `removed > 0`), `move.rs:218-239`.
- **Cause:** unlinking one name of a multiply-linked inode updates that inode's ctime. The plan was fingerprinted before verification; when removal reaches the second name, its identity no longer matches, so removal aborts as "source changed".
- **Probe P4a** (tmpfs → ext4, real `LocalStore`, tree `a`, `plain`, `sub/a-link` = link to `a`): result `Err(SourcePartiallyRemoved)`, `publication_state: Published`, destination complete `["a","plain","sub","sub/a-link"]`, **source left with `["sub","sub/a-link"]`**.
- **Impact:** no data is lost (the destination is verified and complete), but the user is left with a half-deleted source and a NeedsAttention entry whose message does not say the destination is complete. Retry cannot repair it: it re-copies the remainder and then fails with "destination conflict requires an explicit decision" at publish (`operation.rs:865`). Any tree with internal hard links triggers it (pnpm `node_modules`, `cp -al`/rsync `--link-dest` snapshots, deduplicated libraries, Steam/Proton prefixes).
- **Fix:** exclude ctime from the per-file identity when `stx_nlink > 1`, or refresh expected identities of the remaining names of the same inode after each unlink (the plan already knows `(dev, ino)`), or compare size+mtime only for regular files and rely on the exact child-name set for structure. Add a real-FS test with an internal hard-link pair.

#### H2 — High, **confirmed by probe (metadata loss branch) / plausible (failure branch)**: Trash silently degrades to an unverified cross-device copy-and-delete

- **Where:** musheen `mutation.rs:1594-1630` (`move_to_trash` calls `trash::delete` with no capability check); trash 5.2.9 `freedesktop.rs:58-66` (falls back to the home trash when `$topdir/.Trash-$uid` cannot be created), `577-608` (`move_items_no_replace`: on `EXDEV` does `std::fs::copy`/`copy_dir_all` then `remove_dir_all`/`remove_file`, no verification, no fsync), `553-565` (on failure removes the `.trashinfo` but leaves the copied payload).
- **Probe P7** (user namespace: a tmpfs volume whose root is not writable by the user, a user-owned subdirectory, home on a second tmpfs): `move_to_trash` succeeded, `cross_device=true`, payload landed in the home trash, **mtime not preserved, hard-link pair broken (nlink=1), 64 MiB sparse file expanded 4 KiB → 67,108,864 bytes**, source deleted. This is also the path taken for nested btrfs subvolumes (rename returns `EXDEV`).
- **Failure branch (plausible, from crate code):** if `copy_dir_all` fails (ENOSPC on the home partition is likely for large trees) the partial payload stays in `Trash/files` as an orphan without a receipt; if `remove_dir_all(src)` fails part-way (read-only subdirectory, root-owned file, EIO) the source is half-deleted and the complete copy is invisible to any Trash view. That is data loss in practice.
- **Impact:** breaks the project's own rule that the source is removed only after verification (OPS-021 spirit), fills the home partition with the contents of external drives, and makes Trash+Restore lossy. GIO refuses to trash in this situation and offers permanent delete (from my knowledge of `g_local_file_trash`, not re-tested here).
- **Fix:** before calling `trash::delete`, resolve the trash directory the crate will use (same logic as `delete_all_canonicalized`) and refuse with `TrashUnsupported` when it is on another `MNT_ID`/device than the item, or when the mount root is not writable; surface "Delete permanently" instead. Never let the trash path copy bytes.

#### M1 — Medium, **confirmed by probe**: the `RENAME_NOREPLACE` fallback publishes a partial directory under the user-visible name

- **Where:** `operation.rs:895-907` (creates the destination directory, then hard-links children; on error returns `PublishUnknown` and leaves the destination), `915-962`; `copy.rs:873-885` then deletes the staging.
- **Probe P3:** staging `{a, b, zz-locked/secret}` with an unreadable subdirectory and a rename closure returning `EINVAL`: result `Err(PublishUnknown)`, destination exists with `["b","a","zz-locked"]`. In the live flow `cleanup_staging` would then remove the staging (the hard-linked inodes survive in the destination), so the user sees a partial tree with a "publication has an unknown outcome" entry. For moves the source is retained; for copies nothing points the user at the partial tree. Trigger in production: NFS/sshfs (which return `EINVAL` for `renameat2` flags) plus any mid-way error.
- **Fix:** on failure, remove the partially built destination before returning (it only holds links to staging inodes, so this is safe), or publish into a second staging directory and finish with a plain `rename` guarded by a pre-check when `RENAME_NOREPLACE` is unavailable.

#### M2 — Medium, **confirmed by probe**: sparse files inside a moved or copied directory are fully expanded with no warning

- **Where:** `operation.rs:536-555` (`copy_regular_tree_entry` uses `streamed_copy` only, no reflink or `SEEK_DATA` path), `copy.rs:660-665` (`SparseLayout` noted only for a root regular file).
- **Probe P4b:** tree with one 64 MiB file holding 4 KiB of data: move succeeded, `MetadataReport` empty, destination allocation 67,108,864 bytes. A VM-image folder can multiply its disk use by 10-100x and fill the destination; OPS-021 promises sparse preservation "when both stores declare support" and both do (`operation.rs:29-36`).
- **Fix:** reuse `sparse_copy`/`reflink` per tree entry and record `SparseLayout` in `partial_metadata_skips` when it cannot be kept.

#### M3 — Medium, **confirmed by probe**: restoring a trashed symlink that points to a directory always fails and leaves an empty directory at the original path

- **Where:** trash 5.2.9 `freedesktop.rs:396-405` (`file.is_dir()` follows the link, creates a directory placeholder), `420` (`rename(symlink, dir)` → `EISDIR`); musheen `mutation.rs:1671-1679`, `delete.rs:138-146`.
- **Probe P1:** `execute_restore` → `Err(Provider("... IsADirectory"))`, original path now holds a **directory**, receipt still in trash. Every later restore/undo is blocked by that stray directory (`execute_restore` sees an occupant → `Conflict`).
- **Fix:** restore through musheen's own `renameat2(NOREPLACE)` from `Trash/files/<name>` to the original path instead of the crate's placeholder scheme, or special-case symlinks (`symlink_metadata`) before calling the crate.

#### M4 — Medium, **confirmed by probe**: one orphaned `.trashinfo` makes the whole Trash view fail

- **Where:** `mutation.rs:229-258` (`list_trash` stats every payload with `?`, line 238). The crate's `list()` does not check payloads (`freedesktop.rs:74-200`), and its own `restore_all`/failure paths can leave orphans; so can other desktops.
- **Probe P2:** baseline listing ok (210 entries from real per-volume trashes, read only); after adding one info file without a payload: `Err(Missing)`. `start_trash_load` (`app.rs:4401-4420`) turns this into `TrashState::Error`, and `TrashUndo::from_completed/is_available` (`queue.rs:240-275`) return false, so trash undo disappears for everyone.
- **Fix:** skip entries whose payload is missing (report them as "orphaned receipt" with a purge action).

#### M5 — Medium, confirmed by code: full trash listing on the UI thread, once per finished trash job and once per second per undo candidate

- **Where:** `operations.rs:1199-1218` (the finish block runs in `cx.spawn`, i.e. on the foreground executor, holding the queue lock) → `queue.rs:1293-1296` → `TrashUndo::from_completed` (`queue.rs:240-248`) → `list_trash()` (parse every `.trashinfo` on every mount + one `stat` per payload). Then `app.rs:4114-4122` (every ≥1 s while the status center is open, `app.rs:161`) → `refresh_undo_availability` (`15590-15602`) → `can_undo` → `TrashUndo::is_available` (`queue.rs:272-274`) → `list_trash()` again, once per completed trash entry (up to 100 candidates, `queue.rs:1301-1306`).
- **Scenario:** trash 200 files with 20,000 items already in trash: 200 full listings during the jobs, then up to 100 full listings per second while the status center is open. A stalled NFS trash directory blocks the UI thread.
- **Fix:** compute the undo candidate on the worker thread (return the receipt from `execute_detailed`), and check availability by `stat` of the single `.trashinfo`/payload instead of a full listing.

#### M6 — Medium, confirmed by code: unbounded growth of scheduler records, events, and persisted status history

- **Where:** `scheduler.rs:169` (`jobs.insert`, never removed), `507` (`events.push`, never drained; `events()` is only cloned in tests); `status_center.rs:208-235` (`register`, no pruning anywhere; `dismiss` only sets a flag), `537-549` (`to_json` serializes the whole history), `operations.rs:943-965` (`persist_status` clones the whole model on the UI thread on every state change; the worker then writes it twice with four fsyncs via `session.rs:52-78`).
- **Scenario:** every dropped file, trashed file, or rename is one job. After 10,000 jobs `operations.json` is a few MB and is cloned three times per job on the UI thread and rewritten in full; nothing ever shrinks it. `from_json` (`551-579`) bounds custom actions but not entries.
- **Fix:** cap retained finished entries (e.g., 500) when serializing, drop scheduler records for terminal jobs after `finish`, and drain `events`.

#### L1 — Low: `rename_no_replace` has no fallback for filesystems without `RENAME_NOREPLACE`
`mutation.rs:1374-1381`. Rename, hide/unhide (`app.rs:9552-9590`), and rename undo fail with "Invalid argument" on NFS and sshfs, while moves through `try_atomic_move` (`operation.rs:247`) fall back to a full copy of the same-mount item.

#### L2 — Low: cancelling between publish and source removal reports "Cancelled" although the destination copy exists
`move.rs:132-137` returns `after_publish(Cancelled)`; `queue.rs:1240-1245` + `operations.rs:1274` record only `mark_cancelled`. Both copies exist and the user is not told. Local copy/move never call `begin_commit` (only archive create does), so this window is reachable.

#### L3 — Low: trash-restore "Replace" still uses an unjournaled hidden backup
`mutation.rs:327-394`, `425-428` (`.musheen-restore-backup-<pid>-<n>`); a crash between move-aside and restore leaves the previous file under a hidden name that no recovery scans for.

#### L4 — Low: `spawn_ready_hub_operations` can strand jobs as "Running"
`operations.rs:1182-1194`: `start_ready()` already moved jobs to Running in the scheduler; a later `mark_running(id)?` failure returns early and the rest of the batch is never executed or finished. Today the callers keep status and queue consistent (I traced retry, resume_recovery, confirm/keep metadata review), so this needs a lock-poison or a future caller mistake to trigger; it is the same failure shape as old B.3.

#### L5 — Low: `.Trash-$uid` is created with umask permissions
trash 5.2.9 `freedesktop.rs:463` (`create_dir`, typically 0755), so other local users can list the user's per-volume trash. The sticky-bit check applies only to the shared `$topdir/.Trash` (`685-701`), which matches the spec.

#### L6 — Low: startup recovery runs before the first window
`app.rs:2101-2102` builds the hub (status parse, Replace recovery, stale-staging `read_dir` of every directory in history) synchronously inside `run` before windows open at `2125`. A history entry in a directory with a million entries or on a slow mount delays the first window.

### 3. Answers to the questions

**Q2 Undo.** No undo path can overwrite data: rename undo goes through `execute_rename` (identity check + `renameat2(NOREPLACE)`, `rename.rs:44-65`, `mutation.rs:1363-1384`); move undo re-plans through `inspect_drop` (conflict at the original path → `DestinationExists`, no decisions → refused) and executes with `RENAME_NOREPLACE` or staging+`NOREPLACE` publish, with the moved item's identity and the original parent's identity re-checked at execution (`queue.rs:818-848`, `1412-1444`); trash undo checks the original path is empty, the parent identity is unchanged, and the receipt still exists (`queue.rs:263-275`), then the crate creates an exclusive placeholder before renaming (`freedesktop.rs:408-420`). The 11 existing undo tests pass on this checkout. Guards are inode+btime identities (`mutation.rs:1978-1991`), never mtime. Cross-device move undo is a second cross-device move (same H1 caveat). Undo after restart: candidates are in-memory only (`queue.rs:876`), so none is offered; safe. Trash undo when the original folder was deleted: unavailable, and `execute_restore` fails with `Missing` before the crate's `create_dir_all(original_parent)` could recreate it.

**Q3 Trash.** Info files and percent-escaping (`freedesktop.rs:651-676`) are correct for names with `%`, `=`, newlines, and non-UTF-8 bytes. Per-volume `.Trash-$uid` is used when the mount root is writable; otherwise the crate copies across devices (H2). Purge of selected receipts (`mutation.rs:260-282`) is correct and rejects duplicates and unknown receipts. Restore collisions: KeepBoth renames the *existing* file to `X (existing N)` and restores under the original name; Replace is safe but unjournaled (L3); symlink-to-directory restore is broken (M3).

**Q4 New operations.** Duplicate: source identity (dev+ino) checked at submit and execute, names skip queued destinations and existing siblings, publish is `NOREPLACE`, so a race fails cleanly (`queue.rs:1018-1090`); hard-linked sources are copied by bytes (correct for "duplicate"); `archive.tar.gz` becomes `archive.tar (copy).gz` (cosmetic). Links: symlink targets are always absolute (`app.rs:9521`), a dangling occupant at the destination is detected (`statx` NOFOLLOW) and refused, hard links refuse non-regular sources and cross-filesystem targets (`mutation.rs:1392-1457`, `link.rs:122-151`). Templates: regular-file-only at pick time, identity re-checked at queue and execution, copy through `CopySession` (`app.rs:1746-1760`); a swapped template fails with `SourceIdentityChanged`. Hide/unhide: byte-level, refuses already-hidden/not-hidden, `..`-producing names are rejected by `validate_local_name`, conflicts fail through `NOREPLACE` (`app.rs:1762-1782`).

**Q5 Move/copy core.** I found no path where the only complete copy is deleted: publish is verified first, `remove_source` re-fingerprints, and every Replace/merge failure with an uncertain source state keeps both copies (`mutation.rs:1171-1181`, `1264-1278`). Partial destination under a visible name: yes, M1. Staging left under a visible name: no. Directory removed while it still contains uncopied entries: no (child-name sets are compared per directory, `operation.rs:1104-1114`). Hard links inside a tree are preserved (`536-555`) but break removal (H1); symlinks at root and inside trees are copied as links and verified by `read_link`; xattrs/ACLs round-trip tmpfs↔ext4 (P4b report empty); long paths are handled by `/proc/self/fd` in removal but path-based in `copy_directory_tree`/`remove_tree_without_crossing` (fails cleanly with `ENAMETOOLONG`). `RENAME_NOREPLACE` `EINVAL`/`ENOSYS`: handled for publish (non-atomic, M1) and `try_atomic_move` (falls back to a full copy even on the same mount), not for `rename_no_replace` (L1).

**Q6 Startup recovery.** Two instances with different `XDG_RUNTIME_DIR` are stopped by the second (legacy) lock in `$XDG_CONFIG_HOME/musheen/instance.lock` (`instance.rs:22-33`); an older build since `5e9ca8c` holds that same file. Gaps: different `XDG_CONFIG_HOME` (separate status files, so separate recovery scopes anyway), a legacy-lock I/O error that is swallowed (`instance.rs:29`), and builds older than `5e9ca8c`. Recovery blocks the first window (L6). Second launches forward over D-Bus and exit 1 with a message if that fails (`main.rs:13-21`).

**Q7 Persistence.** `atomic_replace` (`settings.rs:480-542`): `create_new` temp with 0600, write, `sync_all`, `rename`, parent fsync, temp removed on failure. One `.bak` generation; a malformed current file never overwrites a good backup (`session.rs:72-76`); settings also keep a `.pre-migration` copy (`settings.rs:205-218`). Downgrade safety is real: newer `schema_version` in current or backup refuses the write (`session.rs:87-111`) and a newer status file makes the hub fall back to in-memory with an error (`operations.rs:359-372`), which also means no Replace recovery runs after a downgrade. Concurrent writers: settings take a `flock` (`settings.rs:166-188`); session/status rely on the instance lock. Read-only or full `~/.config`: errors are surfaced as `persistence_error`, no crash (`operations.rs:143-146`, `960-964`).

**Q8 Bounded resources.** Scheduler limits are enforced where work starts (`scheduler.rs:437-481`), not only in tests; `ResourceLimits` is a validated snapshot per queue. Unbounded: scheduler jobs/events and status history (M6). Undo candidates are capped at 100. Pause/cancel: `wait_if_paused` is correct (`cancel.rs:88-102`, no spin, lock order `pause_lock` → `waiters` only); hub lock order is consistently reservations → queue → status. Pause is not immediate during `verify` (`file_digest`/`tree_digest` never check the token) and never during `remove_source`, by design.

**Q9 Job IDs and status.** IDs continue after the highest persisted ID; interrupted entries are marked at startup (`status_center.rs:185-206`). Stuck "Running" is possible only through L4. Persistence work on the UI thread: the model clone per event (M6) and trash listings (M5); the JSON encode and file writes are on a worker (`operations.rs:95-158`).

### 4. Checked and found clean

- Replace rollback and journal recovery state machine (`mutation.rs:892-1018`), including "neither destination nor backup exists → keep staging" and conflicting markers.
- Merge preflight refuses any non-directory clash, including symlinked directories; merge roll-forward at startup.
- Stale-staging cleanup parses names strictly, protects recorded recovery staging, never crosses mounts, and is skipped when journal recovery fails for that directory.
- `permanently_delete` (fd-based, identity per entry, mount check) and its `ConfirmationRequired` digest.
- `KeepBoth`, `keep_both_destination`, `recovered_original_path` all end in `NOREPLACE` operations.
- Cancellation token, scheduler transitions, retry generation rules (`state.rs:168-189`).
- Status document validation (recovery staging must be app-owned, progress bounds, unknown kinds rejected).

### 5. Probe record

Built with `cargo test -p musheen-local --no-run` in the shared `target-agents` dir (`LIBRARY_PATH` shim for `libacl`). Runs, all against the exported copy only:

| Probe | Result |
|---|---|
| P1 restore trashed symlink→dir | **failed as predicted** (`IsADirectory`, stray directory left, receipt kept) |
| P2 orphaned `.trashinfo` | **failed as predicted** (`list_trash` → `Err(Missing)`) |
| P3 fallback directory publish with mid-way error | **failed as predicted** (`PublishUnknown`, partial destination `["b","a","zz-locked"]`) |
| P4 / P4a tree with hard-link pair, tmpfs→ext4 | **failed, not predicted by the prior audit** (`SourcePartiallyRemoved`, source left `["sub","sub/a-link"]`) |
| P4b tree with sparse file, tmpfs→ext4 | move ok, **64 MiB allocated for 4 KiB of data**, empty metadata report |
| P7 trash from a volume with unwritable root (userns) | **cross-device copy fallback taken**: mtime lost, hard link broken, sparse expanded, source deleted |
| existing undo tests (11) | pass |

I did not run the GUI, did not test NFS/FUSE mounts, and did not touch any real user data (the trash probes used scratch `XDG_DATA_HOME` values; the baseline listing of the user's per-volume trashes was read-only).

### 6. Judgment

The data-safety fixes from the monitoring log are real: 4.1–4.4 and 4.7 hold at `b4b786f`, and I could not construct a copy/move scenario that deletes the only complete copy. The remaining problems are at the edges the previous audit flagged as "partial", plus two things it missed:

1. The removal fingerprint is now too strict in a way that hits ordinary trees (H1): any directory with an internal hard-link pair leaves a half-deleted source after a cross-device move, and retry cannot repair it. This is the same shape as the N1 regression (mtime/ctime on directories) and deserves the same treatment: a real-filesystem test with hard links, sparse files, and symlinks in one tree.
2. The trash path is the weakest link in the whole ops story (H2, M3, M4): the provider hands the item to a crate that copies across devices without verification, breaks restore for symlinked directories, and the listing collapses on one orphan. Trash should be held to the same verify-before-remove rule as move, or refuse.

Everything else is Medium or Low: non-atomic fallback publish, silent sparse expansion, UI-thread trash listings, and unbounded history. Treat cross-device moves of link-heavy trees and trashing from external drives as not safe to release until H1 and H2 are fixed and covered by real-filesystem tests.

---

## Appendix C: desktop, remote, packaging, security (sub-reviewer C)

**Target:** `main` at `b4b786f` (2026-09-24 23:50), read from the exported copy. Note: the checkout itself is now at `988396b`; the 12 commits after `b4b786f` are the `feature/remote-ops` work, already on `main` (`main..feature/remote-ops` is empty). `git diff b4b786f HEAD` shows no change in `privilege/`, `packaging/`, `src/`, `clipboard.rs`, or `file_manager1.rs`, so the findings below also hold at HEAD unless stated.

**Method:** code reading only. I did not build, run tests, launch the GUI, or run pkexec. For library behaviour I read the vendored crate sources in `~/.cargo/registry` (russh 0.63.3, opendal-service-ftp 0.59.3, clipboard-rs 0.3.5) instead of relying on memory. "Confirmed" means I traced the full code path; "plausible" means part of the chain rests on documented platform behaviour I did not execute.

### 1. Status of previous items (5.1–5.10, C.1–C.13)

| Item | Status at b4b786f | Proof |
|---|---|---|
| 5.1 / C.1 caller identity | **Fixed, still in place.** Subject comes from `getppid()` + `PKEXEC_UID` (polkit) or `SUDO_UID` (sudo); JSON subject is discarded. Refuses if both env vars are set, if not root, or if `/proc/<ppid>` uid/start-time do not match. | `bin/musheen-broker.rs:80-94`, `privilege/request.rs:237-270`, `:137-156` |
| 5.2 / C.2 approved ≠ executed | **Mostly fixed.** `--action-id` is argv[1] of the broker, `--request-digest` (blake3 of id+operation) and `--target` are on the pkexec command line and re-checked on stdin (`musheen-broker.rs:77-79,134-174`, `broker.rs:498-508`). Executables must be root-owned, not group/world-writable, with no access ACL (`broker.rs:1085-1120`). Execution is via `/proc/self/fd/N` of the checked descriptor with CLOEXEC cleared first (`broker.rs:1274-1291`), so the earlier "scripts probably fail" concern from `1eebacb` is resolved. Broker path is fixed at `/usr/lib/musheen/musheen-broker` (`broker.rs:25`, moved from `/usr/libexec` in `249958c`; policy, installer and Arch Docker check agree). Remaining: arguments are digest-bound but not shown in the prompt (the UI passes none: `app.rs:8811`). | as cited |
| 5.3 / C.3 unsigned capability | **Fixed.** `ElevatedRootReference` = root + dev + inode, `deny_unknown_fields`; grant id/expiry come from the broker's own authorization. | `rooted_store.rs:12-48, 80-99`, `broker.rs:1248-1254` |
| 5.4 / C.4 no polkit policy | **Fixed in design, still not run end to end.** Three actions, `auth_admin`, active sessions only, `exec.path` + `exec.argv1` annotations. Installed by `install-polkit-policy.sh` (called from `install-app.sh:55`), so the Arch package ships it. No test executes pkexec. | `packaging/polkit/org.musheen.Musheen.policy:8-42`, `install-polkit-policy.sh:9-14` |
| 5.5 / C.5 paste escape | **Fixed.** All control chars except `\n \r \t` are stripped; newlines/controls need confirmation. | `terminal/model.rs:166-190` |
| 5.6 / C.6 volume timeouts | Not re-read this pass. `volumes/` changed by 70 lines since the fix (`udisks.rs`, `model.rs`); I did not review that delta. | `git diff 827c538 b4b786f --stat` |
| 5.7 / C.7 hand-written protocols | **Partly changed.** Real browsing now uses OpenDAL/russh; the hand-written code is only the "Test connection" probe. Probe FTPS is still implicit-TLS only (`probe.rs:283-285, 688`) and now **disagrees with the browsing path** (new finding N1). I found no header line-count limit constant in `probe.rs`. `SystemRoots -> PinnedSha256` still needs no confirmation (`connection.rs:104`). | as cited |
| 5.8 / C.8 zip zstd budget | **Unchanged**, but bounded: the estimate still reads only the first frame header (`zip_codec.rs:224-253`); the decoder is hard-capped at `window_log_max = 27` (128 MiB) (`workspace.rs:20-21, 64-68`), so the under-estimate cannot become unbounded memory. Low. | as cited |
| 5.9 / C.9 extraction refuses links, drops modes, `\` separator, 7z quadratic | **Unchanged.** Links refused at `extract.rs:591, 759`; output 0600/0700 with no mtimes (`:182-183, :194`); `\` is a separator (`path.rs:114, 125`); 7z re-parses the archive for every entry (`seven_codec.rs:191-192`). See N3 for a new cost on top. | as cited |
| 5.10 / C.11 info pane `open::that` | **Still open**, moved to `app.rs:14181`; a second `open::that` at `app.rs:15916` opens the signed release URL. | `git grep open::that b4b786f` |
| C.10 dead code | **Largely addressed.** Create/extract wired (`operations.rs:66`), archive store wired (`providers.rs:857-864`), remote profiles reachable via Network (`providers.rs:118-135`, `providers/remote.rs`), `pool.rs` used by `opendal_store.rs`. `musheen-archive-worker` still exits 1 in default builds (`bin/musheen-archive-worker.rs:8-11`); Arch builds with `--all-features` so it is live there. | as cited |
| C.12 clean list | Re-checked: terminal cwd (`pty.rs:60`), `SecretBuffer` zeroing (`secrets.rs:114-159`), remote errors carry only category+host (`opendal_store.rs:72-74, 440, 599-601`), update verification, thumbnail worker. **Correction:** the `sevenz-rust2` patch is larger than "budget + zeroize + drop zlib-rs": 606 changed lines across 9 source files, mainly a new `ArchiveMemoryBudget` trait threaded through `reader.rs` (455 lines), plus `writer.rs`/`encoder_options.rs` changes. | `diff -ru` against `sevenz-rust2-0.23.0` |

### 2. New findings

#### N1. FTPS "Test connection" and FTPS browsing use different TLS modes and ports (Medium, confirmed)
- **Where:** `remote/probe.rs:283-285` (TLS handshake immediately after TCP connect = implicit FTPS), `:688` (default port 990); `remote/ftp.rs:60-72` (browsing builds `ftps://host` via `profile_endpoint(profile, "ftps", 990)`, which omits the port when it equals 990); `opendal-service-ftp-0.59.3/src/backend.rs:112-113` (missing port defaults to **21**) and `src/core.rs:88-107` (plain `connect`, then `into_secure` = explicit `AUTH TLS`).
- **Scenario:** a profile with no port (or port 990): Test connects to 990 with implicit TLS; browsing connects to 21 with explicit TLS. A profile with port 21: Test does a TLS handshake against a plaintext 220 banner and fails; browsing works. The two paths only agree on servers that offer both modes on both ports. Since saving requires a passing test (`connection.rs:363-383`, `TestRequired`), users either cannot save a working explicit-FTPS server or save a profile whose test passed but which cannot browse.
- **Fix:** make the probe use the same client (OpenDAL operator `check()`/`stat("/")`) or make both agree on explicit `AUTH TLS` on 21 with an explicit "implicit TLS on 990" option.

#### N2. Polkit browsing: one admin password per folder, and listings over 64 KiB stall for 120 s (Medium, confirmed, pre-existing)
- **Where:** `broker.rs:615-636` polls `try_wait()` until exit and only then reads stdout; `elevated_browser.rs:364-376` spawns a new pkexec per request; policy uses `auth_admin`, not `auth_admin_keep` (`policy:14,26,38`).
- **Scenario:** every `ReadDirectory` prompts again. A directory whose serialized listing exceeds the pipe buffer blocks the broker on write; it never exits; the UI reports `ExecutionTimedOut` after 120 s. This was flagged in the monitoring log at `95cc9cf` and is unchanged.
- **Fix:** read stdout concurrently (thread or `wait_with_output` with a watchdog); consider `auth_admin_keep` for `browse-directory`.

#### N3. Extraction copies the archive and decodes every entry twice before extracting (Medium, confirmed)
- **Where:** `extract.rs:96-106` copies the whole source into a temp file in the destination's parent; `:618-631` calls `inspect_nested_entry` for **every regular file entry**, which decodes it into a `NamedTempFile` in the destination parent (`:668-672`) to sniff for nested archives; `:172-223` then decodes everything again into staging. For 7z, each `copy_entry` re-parses from the start (`seven_codec.rs:191-192`), so a solid archive pays two quadratic passes. `decode_limits` sets `max_elapsed` to 30 s (`extract.rs:889`); the tar scanner compares wall-clock time since scanner start on each entry (`tar_codec.rs:156`).
- **Scenario:** a 4 GiB tar.zst costs about 3x the archive size in writes on the destination filesystem before publication, and a scan that takes more than 30 s (easy at that size) fails with a limit error. This is a functional defect, not a safety one: budgets are charged for every byte (`NestedBudgetWriter`, `BudgetWriter`).
- **Fix:** sniff nested archives from the first few KiB of each entry (magic bytes) rather than full decode; snapshot the source only when the source is on a different device or is user-modifiable during the job; make `max_elapsed` a per-entry or idle limit for extraction.

#### N4. Remote credentials cannot be created from the UI (Medium, confirmed)
- **Where:** no code in `crates/musheen-ui` uses `CredentialVault`, `SecretStorage`, `LinuxSecretService` or writes a `secret-service:` reference; the only `SecretBuffer::new` calls are the sudo password (`app.rs:1656-1660`) and volume unlock (`app.rs:1926`). `settings/remote.rs:457-462, 848-855` only read a reference string.
- **Scenario:** a saved FTP/WebDAV/HTTP/SFTP profile can reference a secret, but nothing in Musheen can store one. Password-authenticated browsing works only if the keyring already holds an item with attributes `application=org.musheen.Musheen`, `connection-id=<id>` (`secrets.rs:13-18`), for example seeded with `secret-tool`. Anonymous FTP, unauthenticated HTTP/WebDAV, and credential-less SFTP (see Q9) are the reachable cases.
- **Fix:** add a credential entry in the profile editor that calls `CredentialVault::create` with `SecretStorage::Persistent` (DH-encrypted session is already the default, `secrets.rs:526-531`).

#### N5. The SBOM omits crates enabled only by optional features, but the Arch package builds with `--all-features` (Medium, confirmed script logic; cargo behaviour from documentation)
- **Where:** `scripts/generate-sbom.py:46-60` runs `cargo metadata --locked --offline` with default features and lists `metadata["packages"]`; `:74-78` only checks resolved ⊆ locked, never locked ⊆ resolved. `packaging/arch/PKGBUILD:31` builds `--all-features`, which pulls in `compress-tools`/libarchive, `pavao`/`pavao-sys` (libsmbclient) and the portal backend.
- **Scenario:** the shipped `musheen.cdx.json` and `THIRD_PARTY_LICENSES.md` do not list libarchive or libsmbclient bindings that are in the shipped binary.
- **Fix:** pass `--all-features` (or the exact release feature set) to `cargo metadata`, and add a check that every locked package is either in the SBOM or explicitly excluded.

#### N6. Four vendored forks have no decision record (Medium, process, confirmed)
- **Where:** `Cargo.toml:157-162` patches five crates. Diffs against the crates.io versions: `sevenz-rust2` 606 lines (9 files), `gpui-component` 287 (menu a11y/RTL), `gpui-pre` 48 (a11y/window), `gpui-pre-linux` 81 (Wayland frame loop from `bb3c828`), `native-theme-gpui` 14. Only `native-theme-gpui` has a record in `docs/decisions/`.
- **Risk:** upstream security fixes cannot be rebased without knowing what was changed and why; nobody can tell a deliberate patch from drift.
- **Fix:** one decision record per fork, with the diff summary, upstream issue/PR link, and a re-check date.

#### N7. Second launch during startup can open Home instead of the requested folder (Low, plausible)
- **Where:** `src/main.rs:11-27, 46-65`; `packaging/org.musheen.Musheen.service` (`Exec=/usr/bin/musheen`); `app.rs:2319-2338` claims `org.musheen.Musheen` asynchronously after startup; `apply_file_manager_request` navigates the focused tab for index 0 (`app.rs:4945-4947`).
- **Scenario:** A is starting and holds the lock but has not yet claimed the bus name. B (`musheen ~/Downloads`) sees `AlreadyRunning` and calls `ShowFolders`; the bus daemon sees an activatable name and spawns C (`/usr/bin/musheen`, no args). C sees `AlreadyRunning` and forwards `$HOME`. When A claims the name both queued calls are delivered, so A's focused tab gets `~/Downloads` and then `$HOME`, in daemon order. This rests on standard D-Bus activation queuing, which I did not run.
- **Fix:** have the forwarder pass its own path when activated, or drop the activation service and rely on the lock + forward with a longer retry, or claim the name before heavy startup work.

#### N8. Portal backend interface is callable by any session-bus peer, and nothing routes to it (Low, confirmed exposure)
- **Where:** `portals.rs:557-562, 564-692` export `org.freedesktop.impl.portal.{Request,FileChooser}` under `org.freedesktop.impl.portal.desktop.musheen` when built with `portal-backend` (Arch: yes) and `integrations.portal == "musheen"` (`app.rs:2184-2199`). No `.portal` file is packaged (grep over `packaging/`), so xdg-desktop-portal never uses it.
- **Scenario:** any local process can call `OpenFile` with an arbitrary `app_id`/`title` and receive the user's chosen paths. No privilege gain (same user), but it is a dialog-spoofing surface with no consumer.
- **Fix:** ship the `.portal` file and restrict callers to the portal service's unique name, or drop the feature until it is used.

#### N9. Executable trust relies on the filesystem's reported owner (Low, plausible)
- **Where:** `broker.rs:1110-1112` trusts `uid == 0 && mode & 0o022 == 0`.
- **Scenario:** a user-controlled FUSE filesystem mounted with `allow_other` (needs `user_allow_other` in `/etc/fuse.conf`) can report any uid/mode. The admin still has to approve that exact path, so this weakens defence-in-depth only. `noexec` mounts are safe: exec through `/proc/self/fd` fails on them.
- **Fix:** also require the file to be on a mount without `nosuid` (via `statx` mount id + `/proc/self/mountinfo`) or on a `st_dev` that is not FUSE.

#### N10. "Run" (bca10d2): spawn by path after the check, mode bits only, no origin check (Low, confirmed facts)
- Policy is the global setting `files.executable` = `ask|open|run` (`app.rs:3933-3941`); there is no per-file approval, so nothing is keyed by path/inode/hash and nothing can be inherited. Each run needs the menu's review confirmation (`app.rs:10362-10375`).
- Identity and executable bit are re-checked (`store.resolve_item` + `executable_state`, mode bits only: `musheen-local/src/lib.rs:156-178`), then the file is spawned **by path** (`launch.rs:116-131`), so there is a window between check and exec. Same-user only.
- `.desktop` files and scripts are not special-cased: a `+x` `.desktop` is executed as a program. With modern glibc Rust uses `posix_spawn`, which returns `ENOEXEC` for a shebang-less text file; on the fork/exec fallback glibc's `execvp` would run it with `/bin/sh`. I did not verify which path Rust 1.95 takes here.
- No quarantine or `user.xdg.origin.url` check exists (grep found none). Arguments: none; environment: full inherited environment of Musheen; cwd: the file's parent; `process_group(0)`.
- **Fix:** open the file first and exec via `/proc/self/fd` (as the broker already does), and refuse `application/x-desktop` from Run.

#### N11. A cancelled or denied pkexec prompt is reported as a broker crash (Low, confirmed)
- pkexec exits without output; `decode_broker_response("")` yields `BrokerCrashed` (`broker.rs:307-317, 632-636`), shown as "The administrator broker stopped unexpectedly" (`app.rs:16389`, `locales/en-US.ftl:276`).
- **Fix:** map pkexec exit codes 126/127 to cancelled/denied.

#### N12. Other low items (confirmed unless marked)
- `SystemRoots -> PinnedSha256` needs no confirmation (`connection.rs:104`); pin-to-anything does.
- `FileManager1` validation runs `fs::metadata` on a detached thread with a 2 s race (`file_manager1.rs:255-284`); on a hanging mount the thread leaks per request (plausible DoS by a same-user process; also true of any FUSE-blocking path).
- Every FileManager1 request calls `activate_window` regardless of `startup_id` (`app.rs:2433-2439`); the id is stored but never used. Inherent to the interface.
- Clipboard parse rejects `file://localhost/...` (`clipboard.rs:229-232`) while `FileManager1` accepts it (`file_manager1.rs:386-391`). Foreign paths are not canonicalized; a source ending in `/..` reaches the ops layer with `file_name() == None`; I did not trace what the copy engine does with it.
- Dependency policy: `deny.toml` has no `[bans]`; `verify-dependencies.sh:9-12` runs only advisories/licenses/sources; `verify-dependency-policy.py:11-20` polices six role crates, so the duplicate stacks (`tokio` + `async-io`/`async-executor`/`futures-lite`; `reqwest` + `ureq`) pass. `opendal_store.rs:742-748` owns a private 2-thread tokio runtime and `block_on`s from GPUI threads.
- `scripts/check-smb-provider.sh:7` still hard-codes `/home/shawn/.config/docker-hub/config`.
- `ci/linux-build.Dockerfile:2` is an unpinned tag (release image is pinned, `release.Dockerfile:2`). `aa21730` removed the empty `DOCKER_CONFIG` so `docker build` uses the caller's registry credentials; the build context is `git archive` (`check-linux-build.sh:15-21`), so no credentials or worktree files enter the image. Acceptable.
- `opendal-service-ftp` panics (`expect`) if platform certs fail to load (`core.rs:94-97`); that panic happens inside the private tokio runtime.

### 3. Answers by question

**Q2 Executables:** see N10. Stored as one global preference, not per file. Not via `/proc/self/fd`. The admin path (`RunAsAdministrator`) is the broker, which does use `/proc/self/fd` and root-owned checks.

**Q3 Archives:** traversal is blocked (`path.rs:101-149`: absolute, drive prefix, `..`, NUL rejected; `\` treated as separator; `output_path` re-checks `starts_with(root)`, `extract.rs:839-858`). Symlink/hardlink entries fail the job. Staging is 0700 `create` (not `create_all`), files `create_new | O_NOFOLLOW` 0600. Duplicate paths rejected (`:641-644`). Budgets are enforced on real bytes (`BudgetWriter::write`, `NestedBudgetWriter::write`), ratio 1000, expanded 20 GiB, nesting 8, temp space by `statvfs` (`budget.rs:40-53`). Zip bombs and nested archives are handled, at the cost in N3. **Worker isolation: none.** Extraction runs in-process (`operations.rs:66`) with pure-Rust decoders plus the C `zstd` library; `musheen-archive-worker` is only the libarchive path (RAR/ISO), a plain child process with the archive on stdin (`libarchive_codec.rs:59-103`), no seccomp/namespace. Passwords: `ArchivePassword` is `Zeroizing` (`store.rs:102-137`) and the vendored 7z `Password` zeroizes; the UI never supplies one (`operations.rs:34-40`, `providers.rs:857`), so encrypted archives are unsupported. 7z solid cost: quadratic, twice (N3).

**Q4 Clipboard:** writes `text/uri-list`, `x-special/gnome-copied-files`, `application/x-kde-cutselection` (`clipboard.rs:9-11, 140-144`); no `text/plain`. Paste accepts only `file:///` URIs; one foreign URI fails the whole paste (`:229-232`). Cut vs copy is honoured from Nautilus (first line of the GNOME format) and Dolphin (KDE flag = "1") (`:152-166`); when both are present the lists must match (`:173-178`). Sources are re-resolved against the store before use (`app.rs` `resolve_system_file_clipboard`); the paste destination is the current folder, so a crafted list cannot pick the target directory. Not canonicalized (N12). `clipboard-rs` with `default-features=false, features=["wayland"]` still compiles the X11 backend and picks Wayland when `WAYLAND_DISPLAY` is set (`clipboard-rs-0.3.5/src/platform/mod.rs:70-82`).

**Q5 D-Bus:** exported: `org.freedesktop.FileManager1` under both `org.freedesktop.FileManager1` and `org.musheen.Musheen` (`app.rs:2319-2338`), three read-only methods, `MAX_URIS=256`, 16 KiB per URI and per `startup_id`, 32-deep queue, 2 s per request; `ShowFolders` opens at most 128 tabs per pane (`navigation/pane.rs:5`). No method mutates files. `ShowItemProperties` opens a properties dialog. The portal backend (N8) is the only other interface. Any local process can open folders and raise the window; that is the interface's contract. Activation race: N7.

**Q6 Preview:** "captured files" means the menu's `CommandTargetRef` (id + path) captured at menu time and re-validated against the store before selecting the item and revealing the info pane; directories and replaced files are refused. In-process: only `PreviewDocument` byte reads with clamped limits (1 MiB / 16 MiB / 64 MiB, `preview.rs:7-10, 36-52`). Out of process: anything `MimeDetector` classes as `image/*` goes to `musheen-thumbnail-worker` (`app.rs:16757-16783`), a plain child with a 10 s timeout, 4-worker cap, 64 MiB source, 50 MP / 128 MiB decoded, dimension check before decode (`thumbnail.rs:13-17, 533-581`); no seccomp. Formats: png/jpeg/gif/webp only (`Cargo.toml:58`). SVG goes to the worker and fails cleanly; PDF and video get metadata only. Worker path: `MUSHEEN_THUMBNAIL_WORKER` env or the app binary's sibling (`app.rs:16806-16816`).

**Q7 Terminal:** only `Title`/`ResetTitle` events are consumed (`model.rs:50-62`), so OSC 52 and other side-effect sequences are ignored; query responses (`PtyWrite`) are also dropped, so DA/DSR never get answers (functional gap). Paste: N/A, fixed. cwd is set as process metadata (`pty.rs:60-64`). Environment: the shell inherits Musheen's full environment plus `TERM=xterm-256color` (`pty.rs:65`); shell from `$SHELL` or `/bin/sh` (`profile.rs:95-104`). Output queue 64 x 32 KiB (`pty.rs:12`), transcript bounded (`model.rs:229-327`).

**Q8 Updates:** Ed25519 public key embedded (`updates.rs:12-15`); tests use their own seed (`tests/updates.rs:19-24`), so the project private key is not in the repo. Signed message covers schema, channel, sequence, version, URL, expiry (`:256-264`); channel must equal the policy channel (`:205-207`); per-channel sequence store with `flock` + atomic replace rejects lower sequences as `Replay` (`state.rs:40-65`); expiry checked (`:271-273`). HTTPS-only, 5 s, 1 MiB. **Nothing is downloaded or executed** (`can_install()` is `false`, `:140-144`); the only action is showing a link. Opt-in via `advanced.updates` (`app.rs:2461-2471`); URL is hard-coded `https://updates.musheen.app/v1/latest.json`; I could not verify who owns that domain.

**Q9 Remote:** credentials live in Secret Service (DH session by default, `secrets.rs:526-531`) or session-only memory (`SecretBuffer`, zeroized); settings hold only `secret-service:<id>` references (`connection.rs:616-668`). No UI to create them (N4). TLS: rustls with native roots, or a SHA-256 pin over the leaf certificate DER that keeps signature verification (`opendal_store.rs:159-176`; probe `probe.rs:535-560`); pinned FTPS is refused rather than downgraded (`ftp.rs:63-67`). SSH: `KnownHosts` is strict with no TOFU or prompt (`sftp.rs:722-724`), so a new host fails until it is in `~/.ssh/known_hosts`; pin = SHA-256 of the wire-format key blob (`russh-0.63.3/src/keys/mod.rs:238-242`), which matches `ssh-keygen -lf`. Credential-less `KnownHosts` SFTP profiles go through OpenDAL's sftp service, which spawns the system `ssh` with `StrictHostKeyChecking` (`sftp.rs:67-88`); all others use russh with password or unencrypted PEM key (`:750-767`). FTP plaintext and HTTP Basic need `PlaintextConfirmed`. Errors expose category + host only. Timeouts: probe 15 s, OpenDAL 30 s per request, 60 s transfer idle, SFTP connect 10 s (auth has no own timeout beyond `run_remote`). Reachable from the UI now: saved FTP/FTPS/WebDAV/HTTP/SFTP profiles under Network (`providers/remote.rs:15-76`); SMB and NFS return unsupported (`:60-71`), so `pavao` is compiled into the Arch build but unreachable.

**Q10 Packaging:** no setuid anywhere (`install -m 0755`, `install-app.sh:31-33`, `install-polkit-policy.sh:9-11`); the broker relies on pkexec/sudo. `org.musheen.Musheen.service` is installed (N7); the FileManager1 activation service was correctly removed in `37460c0`. Docker credentials: see N12. `deny.toml` covers advisories (five dated `Expires 2027-03-31` ignores, all "unmaintained" notices), licenses, and one LGPL exception; `sources` runs with defaults. SBOM: N5. Duplicate stacks and policy gaps: N12. `jiff` 0.2.35 reads the system zoneinfo on Linux; no concern. `pavao` is optional C FFI, unreachable from the UI. Vendored forks: N6.

**Q11 Instance lock:** `$XDG_RUNTIME_DIR/musheen/instance.lock` when that variable is an absolute path, otherwise `~/.config/musheen/instance.lock` (`instance.rs:36-43`); there is **no `/tmp` fallback**. The lock file is opened `O_RDWR|O_CREAT|O_CLOEXEC|O_NOFOLLOW` mode 0600 and `flock`ed non-blocking (`:52-70`). The directory is created with `create_dir_all` (default mode under umask), which is fine inside the user's 0700 runtime dir or home config. Clean.

### 4. Checked and found clean
- Broker identity binding, ppid semantics (pkexec `execv`s in place), refusal when both `PKEXEC_UID` and `SUDO_UID` exist, refusal when not root, environment scrubbing to `LANG`/`TERM`/`LC_*` with a strict charset (`broker.rs:1207-1218`), audit log at `/var/log/musheen/privilege.jsonl` 0700/0600 under a root-only parent, before/after identity check around authorization (`broker.rs:1026-1036`), `open_absolute_no_symlinks` walk with `O_NOFOLLOW|O_DIRECTORY` per component.
- Sudo PTY transport: request withheld until the readiness marker, password limited to 1 KiB with no CR/LF/NUL, `env_clear`, zeroized after write.
- Polkit policy: `allow_any`/`allow_inactive` = no; `exec.argv1` matches the launcher's argument order (`broker.rs:498-503` inserts `--action-id=` at index 2, which is the broker's argv[1]).
- Clipboard URI encoding/decoding, NUL rejection, format cross-check.
- `FileManager1` URI parsing, NUL rejection, size limits, read-only method set.
- Update signature ordering, replay store locking, HTTPS enforcement on both metadata and information URLs.
- `SecretBuffer` redaction and zeroing; `ConnectionProfile`/`ProxySettings` `Debug` redaction; profile import limits (1 MiB, 256 profiles, `deny_unknown_fields`).
- Pinned verifiers keep handshake signature verification; pinned FTPS refuses instead of downgrading.
- Thumbnail cache writes: `create_new` temp + rename, failure records sanitized to ASCII and 1 KiB.
- Terminal paste sanitizing and transcript bounds.
- Packaging modes; `DESTDIR` symlink/root checks; release image digest pin; Arch build as unprivileged `builder` with `--locked --offline`.

### 5. Unmerged branch `feature/remote-ops`
The branch is fully merged: its 12 commits (`11de530`..`988396b`, +3,936 lines) are on `main` after `b4b786f`. They add a remote transfer capability model (`musheen-ops/src/remote.rs`), local-to-remote uploads and downloads (`providers/remote/transfer.rs`, 10 GiB local staging cap checked with `statvfs`), remote-to-remote relays with 1 MiB verification chunks (`relay.rs`), and OpenDAL operations that only publish with `if_not_exists` and only delete a moved source with an `if_match(etag)` condition, refusing backends that lack those capabilities. That design is sound on paper. Obvious risks I would want checked before trusting it: the private 2-thread tokio runtime driven by `block_on` from GPUI threads (deadlock or starvation under concurrent transfers), ETag stability across FTP/SFTP where OpenDAL synthesizes weak identities, and 60 s idle timeouts that abort large uploads over slow links with staging left behind for "recovery" that the UI may not surface. I did not review it in depth.

### Judgment

The privilege broker is in good shape: the caller identity, target binding, executable trust, and polkit policy fixes from 5.1–5.4 are all present at `b4b786f` and unchanged at HEAD, and the earlier worry about scripts through `/proc/self/fd` is resolved. What remains there is functional: nobody has run an elevated action end to end, the polkit path prompts per folder, and large listings stall. No custom crypto, no setuid, no secrets on disk in plaintext, and every new local surface (Run, Preview, clipboard paste, FileManager1) re-validates its target before acting.

The real defects this pass found are correctness and delivery gaps rather than boundary breaks: FTPS test and browse cannot agree (N1), extraction does three times the I/O it needs and can time itself out (N3), remote credentials cannot be entered anywhere (N4), and the SBOM does not describe the binary the Arch package ships (N5). The process debt is the five vendored forks with one decision record between them (N6). Fix N1, N4, and N5 before calling remote support or the release pipeline delivered; fix N3 and N2 before calling archives and elevated browsing usable; write the four missing fork records now, while the reasons are still known.

---

## Appendix D: spec fidelity, parity, process (sub-reviewer D)

**Baseline note.** The task named HEAD `b4b786f` (2026-09-24 23:50). The repository has moved on: HEAD is now `988396b` (2026-09-25 02:10), 12 commits later, all `feat(remote)`/`feat(ops)`. I classified all 118 commits in `6bc54bd..HEAD` and read code at current HEAD; no file in that range except the remote work differs from `b4b786f`.

**What I ran and did not run.** Read-only throughout. I used `/usr/bin/git`, `/usr/bin/grep`, `diff -ru` against `~/.cargo/registry`, `docker images`, `docker buildx history ls/inspect/logs` (read-only), and `ls` of the scratchpad output directories. I did **not** run cargo, the benchmarks, or the GUI: the machine had load average 24 with 10 rustc processes and no shared target dir existed yet, so a bench run would have taken a long time with no guarantee of finishing. Two Explore sub-searches gathered the feature table (Q5) and the test census (Q7); I re-verified their headline claims in code (Extract refusal, SMB/NFS refusal, credential entry, `check-budgets.sh` test names). I did not open the Files 4.2.9 source; parity is judged against `docs/spec/features.md` and the prior audit's Files feature list. I did not look inside `scratchpads/musheen-target`, so I cannot see host-side `cargo test` runs; only Docker runs leave a record I can read.

---

### Q1. Order of work

**Commit counts, `6bc54bd..HEAD` (118 commits, 2026-09-23 10:12 to 2026-09-25 02:10):** 43 fix, 34 feat, 14 test, 10 perf, 8 build, 7 docs, 1 ci, 1 chore. Scopes: 44 unscoped, 34 `ui`, 10 `remote`, 8 `ops`, 4 `session`, the rest 1-2 each.

**Classification against the agreed order** (counts approximate ±2, a few commits straddle two steps):

| Agreed step | Commits | Examples |
|---|---|---|
| 1. Data-loss/recovery | 0 | finished before 10:00 (monitoring log) |
| 2. Wire folder open, selection, clipboard, rename, trash, new folder, shortcuts | ~17 | `c1c68f7` system clipboard 10:31; `f9600ca` **inline rename only on 09-24 18:08**; `05b5cdb` hide, `cfb4019` links, `b3a1839` duplicate, `8ee745d` templates 19:37-22:01 |
| 3. Startup load, paging, dates, density, Linux styling | ~31 | `96775f5` density/shortcuts 10:12; `bb4a743`, `36a0384`, `ea434a4`, `6ceca1a` sidebar; disk-backed index `9f5ecd3`..`3c3af25` (18 code + 5 docs commits, 09-24 04:27-10:49) removes the 100k cap |
| 4. Verify in the real GUI | **0** | no commit, screenshot, or note (see Q9) |
| 5. Archive/remote (only after 4) | ~15 | `6bc54bd` archive store 10:01 (boundary commit), `a572eae` 15:59, `af93ad1`, then `b4b786f` + 12 remote commits 23:50-02:10 (~4,200 lines) |
| Not in the agreed order: phase-7 release hardening | **~44** | packaging/CI 19 (`fe4c2b2`, `249958c`, `8b268bd`, `3487631`...), migrations/downgrade 8 (`83f797a`..`30d0f6a`), limits 6, fault matrix 2, benchmarks 9 (`92ef19c`..`9ce291a`, `47051b7`) |
| Not in the agreed order: new features | 6 | undo `d8348e6`, `9e1629a`, `0a1cbe6`, `bb50edb`, `e169165`; Miller columns `0f64f7f` |
| Misc (flaky-test fixes, fmt, matrix docs) | ~7 | `a162c5d`, `0acb72d`, `4817b50`, `bcb2b07` |

**Verdict: the order was not followed (High, confirmed).** About 40% of commits went to steps 2-3, 0% to step 4, and ~50% to work that was either explicitly deferred (archive/remote) or never agreed (release hardening, undo). Step 5 started at 10:01 and 15:59 on 09-23 while inline rename, links, duplicate, hide and templates (step 2) did not land until the evening of 09-24. Step 4 has never been done. After `bca10d2` (09-24 22:53) Codex went straight into 13 remote commits.

**Commits per hour.** 37 of the 40 clock hours had at least one commit; peak 7/h (09-25 00:00), 6/h at 09-23 22:00 and 09-24 06:00; mean 3.2/h. Longest gaps: 09-23 12:36-15:30, 09-24 10:49-12:12, 09-24 16:36-18:08.

**Runs that look like activity for its own sake:**

1. `92ef19c`..`9ce291a`, 09-24 13:21-15:09: 8 `perf: measure` commits, ~2,130 lines, in 108 minutes. None of these benchmarks has ever been executed (Q3). Each also grows `tests/release_runner.rs` with fake-benchmark fixtures that test the script that would run them.
2. `3487631` → `7201f8e` (1 minute later, "make release matrix Dockerfile parseable") → `041f022` (4 minutes later), 09-23 22:48-22:53, matched by three failed Docker builds at 22:48, 22:50, 22:53 (buildx history). The Dockerfile was committed before it was parsed.
3. `2017a7c`..`30d0f6a`, 09-24 01:50-03:55: 8 downgrade/migration-protection commits for a 0.1.0 app that has never shipped a schema to anyone.
4. `9f5ecd3`..`3c3af25`, 09-24 05:43-10:49: 18 index commits at ~15 minutes each. Real functionality, but the failure/permission tests arrive last (`0ddd137` 10:04), the reverse of the plan's failing-test-first steps, and none of the plan's six prescribed commit messages was used.
5. `11de530`..`988396b`, 09-25 00:02-02:10: 12 remote commits, ~4,200 lines, with no Docker verification build in the buildx history after 22:53.

All 118 commit messages have **empty bodies** (`git log --format=%b` yields 118 blank lines): no commands run, no requirement IDs, no screenshots, although `AGENTS.md` asks for all three.

---

### Q2. Plans vs code

**Checkbox state (unchanged since 2026-09-22; no tick commit exists in this range):**

| Plan | Checked / open | Reality |
|---|---|---|
| 01 foundation | 0 / 43 | built (prior audit) |
| 02 browse and inspect | 0 / 36 | built (prior audit) |
| 03 safe local operations | 11 / 26 | tasks 1-4 built but unticked |
| 04 commands and customization | 41 / 0 | |
| 05 Linux desktop integration | 41 / 0 | |
| 06 archives and remote | 18 / 21 | tasks 4-6 now largely built (OpenDAL, SMB stub, remote ops) but unticked |
| 07 release hardening | **0 / 41** | ~44 commits implement parts of tasks 1, 2, 3, 5, 6 |
| 2026-09-24 disk index | **0 / 25** | 18 commits implement tasks 1-6 |
| `docs/commitments/foundation.md` | no boxes | still says the foundation "adds no user-facing application behavior" |

**Sampled checked items (26), with code and test evidence:**

| Plan / item | Verdict | Evidence |
|---|---|---|
| 03 T5 test replace/skip/keep-both/merge/apply-to-all | proven | `crates/musheen-ops/tests/conflict.rs:31`; `crates/musheen-ui/tests/drop_operations.rs:182,247,310,399` |
| 03 T5 conflict decisions journaled | proven | `crates/musheen-desktop/tests/conflict_journal.rs` (exists; not read line by line) |
| 03 T5 progress + status center | partial | code `app.rs:15677-15863`; no test found that drives the status center |
| 03 T5 Trash surface with original location and time | proven | `crates/musheen-ui/tests/operations.rs:165` |
| 03 T6 "run every operation test under fault injection" | unproven | no run record anywhere |
| 03 T6 "inspect all deletion/overwrite call sites" | unproven, and wrong at the time | its artifact `docs/safe-local-operations-audit.md` cites 3 tests that do not exist and a Replace rule that the prior audit showed was false (B.1) |
| 04 T1 command registry tests | proven | `crates/musheen-core/tests/command_registry.rs` (1,352 lines) |
| 04 T2 context menu table tests | proven | `crates/musheen-ui/tests/context_menus.rs` (2,084 lines) |
| 04 T3 settings window tests | proven | `crates/musheen-ui/tests/settings.rs` (1,037 lines) |
| 04 T4 "manually verify mouse-free editing" | unproven | no record |
| 04 T5 themes and safe custom actions | proven | prior audit; `crates/musheen-ui/tests/customization.rs` |
| 04 T6 tags/pins/home | proven | `crates/musheen-desktop/tests/catalog.rs`; prior audit |
| 04 T7 command-surface matrix "verified every projection" | partial | matrix exists; `archive.extract` and `directory.share` are listed live but refused (Q8) |
| 05 T1 MIME cascade | proven | `crates/musheen-desktop/tests/mime.rs` |
| 05 T2 "one supported-desktop integration smoke test" | unproven | `crates/musheen-desktop/tests/volumes.rs:1026-1030` returns silently when UDisks is absent |
| 05 T3 FileManager1 | proven | `crates/musheen-desktop/tests/file_manager1.rs` (private `dbus-daemon`) |
| 05 T4 Secret Service | proven | `crates/musheen-desktop/tests/secrets_zbus.rs` |
| 05 T5 "implement Polkit authorization for a typed broker request" | partial | now delegated to `pkexec --provider=polkit` (`privilege/broker.rs:478-488`); the in-process D-Bus check and `tests/polkit_zbus.rs` were deleted in `95cc9cf` |
| 05 T6 terminal | proven | prior audit |
| 05 T7 "review every process/URI/D-Bus/credential/privilege boundary" | unproven, and wrong at the time | six issues (5.1-5.6) found by the audit and fixed 09-23 |
| 06 T1 archive store | proven | `desktop/src/archive/store.rs`; now reachable (`6bc54bd`) |
| 06 T2 create/extract "through the operation engine" | partial | Compress and Extract Here work; "Extract…" is always refused (`app.rs:7828-7843`) |
| 06 T2 bomb/crash/cleanup fixtures | proven | `crates/musheen-desktop/tests/archive_operations.rs` (3,797 lines) |
| 06 T3 connection profiles and pools | proven | `crates/musheen-desktop/tests/remote_pool.rs` (1,497 lines) |
| 06 T3 connection-test and save flow | partial | exists in `settings/window.rs`; no credential entry (Q5) |
| 06 T3 timeout/saturation fixtures | proven | `remote_pool.rs` `pool_caps_capacity_cancels_waiters...` |

**Ratio: 16 proven (62%), 5 partial (19%), 5 unproven (19%).** Both "unproven, and wrong at the time" items are review/close-out ticks. The reverse error is now larger than the forward one: plans 07 and the index plan carry 0 ticks against ~62 implementing commits, so the checkboxes no longer track anything.

**`2026-09-19-07-release-hardening.md` specifically:** Task 1 mostly exists (`verify-msrv.sh`, `verify-dependencies.sh`, `run-release-matrix.sh`, `release.yml`, `deny.toml`, SBOM). Task 2 "complete fault matrix" over 12 boundary classes: `tests/fault_matrix.rs` (107 lines) covers exactly one boundary, `provider.read_directory`. Task 3: benches exist, `docs/performance-baseline.md` does not, nothing has run (Q3). Task 4: `scripts/check-ui-baselines.sh` does not exist; the PNG baselines are stale (Q9). Task 5: done and exercised in Docker (Q4). Task 6: `tests/migrations.rs` has 6 tests; `scripts/release-upgrade-test.sh` does not exist. Task 7 (the gate): not started.

**`2026-09-24-disk-backed-directory-index.md` specifically:** written by Codex at 04:27/04:42 and executed from 05:43 with no approval record. Of the five prescribed test names, two exist (`external_order_supports_random_ranges` at `directory/index.rs:925`, `streaming_directory_model_pages_through_one_million_items` at `tests/shell.rs:231`) and three do not. The million-record verification the plan requires, `external_order_supports_one_million_records` (`index.rs:1199`), is `#[ignore]` and nothing passes `--ignored` for it. Task 6's "exactly one local Docker package build" did happen (09-24 10:40). The plan's architecture rule was edited the same morning (`b0e7c3d`) to permit `musheen-ui` file I/O for this index; the previous rule was "No crate above `musheen-core` may call `std::fs` directly." No decision record covers that change.

---

### Q3. Performance claims

**What the benchmarks are.** Eight `harness = false` programs under `benches/` that build synthetic fixtures (`MillionItemFixture`, fake stores, PTY floods), sample `/proc/self/schedstat`, `VmHWM`, `/proc/self/fd`, and temp-dir bytes before and after, and print one JSON line per case. `benches/startup.rs` launches the real binary under X11 and polls `xwininfo`. This is real measurement code, not stubs.

**Have they run? No (High, confirmed).**
- The only invocation path is `scripts/run-benchmarks.sh`, called only from `ci/release.Dockerfile` after the full 8-entry release matrix and `check-budgets.sh`.
- Docker history: the one **completed** `ci/release.Dockerfile` build started 2026-09-23 23:45 local (25m49s, artifacts `musheen.cdx.json` and `THIRD_PARTY_LICENSES.md` in `scratchpads/musheen-release-K41Yxd`). That predates `d6894f7` (budgets, 09-24 12:31) and the perf series (13:21+). The only release build that contained the benchmarks started 2026-09-24 16:02 local and was **canceled after 7m27s** at step 15/17.
- No numbers are committed anywhere: `docs/performance-baseline.md` (required by plan 07 task 3) does not exist; no JSON/JSONL output is tracked; `git grep wall_ns` hits only the bench code and its tests.
- `tests/bench_metrics.rs` (33 lines) tests the `/proc` text parsers with made-up strings. `tests/release_runner.rs` runs `run-benchmarks.sh` with a **fake `cargo` and fake bench binaries**. These prove the plumbing, not a measurement.

**Do they check `docs/spec/limits.md`?** Partly. `scripts/benchmark-validations.jq` requires `queued_pages_max <= 2`, `retained_models_max <= 4096`, `queued_matches_max <= 2048`, `pool_connections_max <= 8`, and so on, which mirror LIMIT-002/003/007/008. But: (a) there is **no threshold on `wall_ns`, `cpu_ns`, or `peak_rss_kib`** (`nonnegative(...)` only), so no performance regression can fail; (b) some "measured" counters are literals, for example `"queued_pages_max": 1` and `0` in `benches/directory.rs`, so the jq check would pass by construction; (c) `startup` honestly reports `internal_counters_sampled: false`. `limits.md` itself says the values are "not benchmark targets", which is consistent, but then the "perf: measure" commits measure things nobody has looked at.

**`check-budgets.sh` and `18274e5` (Medium, confirmed).** All 20 named tests exist. Most are behavior tests (`pty_output_backpressures_a_slow_consumer_without_losing_bytes`, `selection_reads_only_the_initial_mebibyte_of_a_sparse_tibibyte_file`). Two are constant mirrors: `default_directory_limits_match_the_production_budget` repeats the seven literals in `Default for ResourceLimitConfig`, and `directory_retention_hard_max_matches_the_resident_model_cap` asserts `MAX_DIRECTORY_RETAINED_ITEMS == 4_096` without touching the real private cap `MAX_RESIDENT_ITEMS` at `crates/musheen-ui/src/directory.rs:15`. `18274e5` loops over all seven fields asserting `MAX + 1` is rejected, which is a real validation check, but nothing asserts that `MAX` itself is accepted, and the bounds come from the same constants, so no absolute number is verified. Fix: run the suite once in the container, commit the baseline with the image digest, add RSS/wall ceilings, and compare the UI cap against the core constant in one test.

---

### Q4. Release and packaging claims

**Docker history (times converted to local, UTC-4):**

| Build | Runs | Result |
|---|---|---|
| `ci/arch-package.Dockerfile` (`build-arch-package.sh`) | 21 between 09-23 21:26 and 09-24 10:40 | 15 completed, 6 errors. Eleven `musheen-0.1.0-1-x86_64.pkg.tar.zst` (27.7-27.8 MB) survive under `scratchpads/musheen-arch-package-*` and `musheen-arch-ci-*/`, first 09-23 22:33, last 09-24 10:43. The 10:32 failure was `musheen-ui --test volumes` in `check()`, fixed by `1ddd63c` at 10:49. |
| `ci/release.Dockerfile` (`run-release-matrix.sh`) | 6 between 09-23 22:48 and 09-24 16:02 | 1 completed (09-23 23:45, 25m49s). Its log shows all 8 "Release matrix: Rust {1.95.0,stable}, {minimal,all} features, {debug,release}" entries and no failing test. The 34-minute 23:09 run passed the matrix and failed only at the SBOM step. The 09-24 16:02 run with budgets and benchmarks was canceled. |
| `ci/linux-build.Dockerfile` (`check-linux-build.sh`) | ~20 | runs for most 09-23 afternoon/evening commits and for 09-24 19:05-22:53; the 22:02 run failed `workspace_boundaries`, fixed by `e4121ce` at 22:07. **No run for the 20 commits 09-24 10:49-18:52 (limits, benches, rename) except the canceled release build, and none for the 13 remote commits after 22:53.** |

**`8b268bd` "prove Arch package opens a graphical window".** The commit adds an Xvfb + `xwininfo` smoke step to the Arch Dockerfile and a test that asserts the Dockerfile **text** contains `xorg-server-xvfb` and `package-launch-smoke.sh`. The real proof exists only in Docker: the last completed Arch build log (09-24 10:40) contains `native graphical launch passed`, the `namcap` step, and the `pacman -Rns` removal step. So the claim is true for the tree as of 09-24 10:40 and untested for the 45 commits since. A window that maps under Xvfb is not evidence that anything in it works.

**What the root tests do without Docker.** None skips silently; every external call is `unwrap()`. But none runs Docker either: `release_runner.rs` and `linux_build_script.rs` put a **fake `docker`** first on `PATH` that records its arguments; `release_matrix.rs`'s budget tests use a **fake `cargo`**; `desktop_packaging.rs` runs the real `install-app.sh` into a temp `DESTDIR` with a fake `rsvg-convert`, and its PKGBUILD/Dockerfile tests assert substrings; `remote_provider_ci.rs` only counts `"docker run"` occurrences in a script. These are plumbing and text checks. Two silent skips exist elsewhere: `desktop/tests/remote_live_contract.rs:92` passes unless `MUSHEEN_REMOTE_LIVE` is set, and `volumes.rs:1028` returns without UDisks.

**`docs/spec/roadmap.md`** still says `Current: foundation` (Low, confirmed). `.github/workflows/release.yml` is tag-triggered only; I did not check whether any tag exists.

---

### Q5. Feature parity vs Files 4.2.9

Verified in code by the sub-search; I re-read the three most important refusal sites myself. Musheen now backs 83 of 84 `CommandAction` variants in the static table (`app.rs:10511-10603`), up from 57 at the first audit. `Share` is the only table-level refusal.

| Feature | Status | Evidence |
|---|---|---|
| Tabs, dual pane | reachable, works | `command.rs:1529-1628`, `app.rs:11573-11630` |
| Details/List/Grid/Cards/Columns/Adaptive | reachable | Columns is now a real 3-parent Miller view (`views/columns.rs:9`, `app.rs:14504-14560`); Cards = wider Grid |
| Preview pane | partial | Text, binary, image thumbnail, details (`info_pane.rs:46-51`); no PDF/video/audio; local only |
| Search with filters | partial | typed tokens only (`musheen-core/src/search.rs:101-123`); no filter chips, no date picker, no `tag:` token |
| Tags | reachable | Properties Tags page; sidebar section; filter limited to loaded items of the current folder (`app.rs:12901`) |
| Properties | reachable | General, Permissions, Open With, Tags, Checksums; no details/media page |
| Archives | partial | Browse opens a **separate read-only window**; Compress = zip in place; Extract Here works; **"Extract…" always refused** (`app.rs:7828-7843` accepts only `copy_to`/`move_to`) |
| Cloud drives | absent | no detection code |
| Git integration | absent | not specified |
| Status center | reachable | `app.rs:15677-15863`, with pause/resume/cancel/retry/undo |
| Settings pages | reachable | 9 pages (`settings/document.rs:5-15`) |
| Shortcut editor, custom themes | reachable | `settings/shortcuts.rs`, `settings/appearance.rs` |
| Clipboard incl. other apps | reachable | `text/uri-list` + `x-special/gnome-copied-files` (`desktop/src/clipboard.rs:26-86`) |
| Inline rename | reachable | `app.rs:8435-8443` (since `f9600ca`) |
| Batch rename | **in code, not reachable** | `musheen-ops/src/batch_rename.rs`; no UI reference; Rename requires exactly one item |
| New file/folder/template | reachable | template = any file via picker; no `~/Templates` menu |
| Trash, permanent delete, restore, empty | reachable | `app.rs:8477-8537` |
| Undo | partial | per-job button in status center only (`app.rs:15791-15799`); no Ctrl+Z; rename, simple local move, trash only |
| Duplicate, symlink, hardlink, hide | reachable | local only |
| Remote FTP/FTPS/SFTP/WebDAV/HTTP | **reachable but not usable for password logins** | profiles from Settings appear under Network (`providers.rs:117-154`); **no credential input exists**: `settings/window.rs:1154` asserts `!inputs.contains_key("remote.credential")`, `settings/remote.rs` stores only a `CredentialReference`, and `providers/remote.rs:92` reads existing Secret Service items. The user must create the secret with another tool. |
| SMB, NFS | refused | `providers/remote.rs:134-145` ("not yet available", "must be mounted by the system first") |
| Network location | partial | lists saved profiles only; no discovery |
| Open with, run as administrator, terminal | reachable | |
| Drag and drop | in-app only | no external-drop handler |
| Shelf | absent | |
| Thumbnails in Grid/Cards | **in code, not reachable** | worker used only by the preview pane (`app.rs:16768-16795`); grid shows type icons |
| `Mount`, `Unlock` commands | no surface | in no menu list; contextual commands without a menu entry are dropped silently (`app.rs:5468-5471`) |

**Top 15 missing or unreachable user-facing features:**

1. Extract to a chosen folder ("Extract…") — refused after the picker (High for an archive feature the matrix lists as live).
2. Remote credential entry — no way to enter a password or key passphrase in the app; makes most of the 13 new remote commits unusable without `secret-tool`.
3. SMB browsing — refused.
4. Batch/bulk rename (Files F2 dialog) — backend only.
5. Thumbnails in Grid/Cards views — backend only.
6. Image, PDF, video, audio preview beyond a thumbnail.
7. Undo as a command (Ctrl+Z), and undo for copy, delete, create, extract.
8. Network discovery (SMB/mDNS neighbourhood).
9. Drag and drop from other applications.
10. Cross-folder tag view (Files lists all files with a tag; Musheen filters the current folder).
11. Search filter UI (chips, date and size pickers) and `tag:` token.
12. Archive browsing in the current tab instead of a separate window.
13. Cloud drive detection (OneDrive/Google Drive/Dropbox rclone or gvfs mounts).
14. Share, Mount and Unlock commands — present in the registry with no working surface.
15. New-from-template submenu of `~/Templates`; Properties details/media page.

Git integration and the Shelf remain absent and were never specified.

---

### Q6. Decision records and forks

`docs/decisions/` holds two files: the OpenDAL SFTP blocker and the native-theme patch. Only the native-theme fork has a record ("Level: Judged, Decided by: agent", with a removal condition).

| Vendored crate | Lock version = vendor version | Changed lines vs registry copy | What is patched | Record / upstream plan |
|---|---|---|---|---|
| `sevenz-rust2` | 0.23.0 | **591** (10 files + `Cargo.toml`: adds `zeroize`, drops `zlib-rs` feature) | `ArchiveMemoryBudget` trait and lease, `MemoryLimitExceeded` error, budgeted PPMd/LZMA dictionary sizes, password zeroing; 5 commits 09-22 to 09-24; it is also a workspace member so its 91 tests run in `--workspace` | none |
| `gpui-component` | 0.6.4 | 287 (4 menu files) | AccessKit roles/aria on menu items, `PopupMenuDirection` for RTL, `Rc::downgrade` to fix a menu-state cycle; 4 commits 09-20 | none |
| `gpui-pre-linux` | 0.3.5 | 81 (1 file) | Wayland frame-loop `Retry` state so a `notify` while awaiting a compositor callback still schedules a frame; unit tests included; 1 commit `bb3c828` 09-23 | none; this is the startup-load fix and a genuine upstream bug |
| `gpui-pre` | 0.3.5 | 48 (4 files) | `aria_has_popup`, `aria_disabled`, a11y debug JSON, test-only accessibility activation; 2 commits 09-20 | none |
| `native-theme-gpui` | 0.5.8 | 14 | remove two `tiles` assignments for gpui-kit 0.6.2+ | record exists |

Finding (Medium, confirmed): four forks with ~1,000 patched lines have no decision record, no upstream issue or PR link, and no removal condition. `AGENTS.md` mentions `vendor/` only as a location. Fix: one record per fork stating the reason, the diff scope, the upstream ticket, and the condition for dropping the patch; file the Wayland and menu-a11y patches upstream since they are general fixes.

---

### Q7. Tests as evidence

**Census (attribute counts, from the sub-search; not run):** 1,199 test attributes: 1,041 `#[test]` and 158 `#[gpui_kit::test]` (headless window; per gpui-kit's own `test.rs` it "does not inspect rendered pixels").

| Crate | src tests | tests/ | notes |
|---|---|---|---|
| musheen-core | 2 | 50 | pure |
| musheen-local | 46 | 37 | ~all use real temp filesystems |
| musheen-ops | 0 | 84 | **all in-memory fakes**, no real files |
| musheen-desktop | 33 | 357 | ~294 touch real files; 33 start a private `dbus-daemon` (portals, FileManager1, secrets, UDisks); 1 uses the real system bus and returns silently without UDisks |
| musheen-ui | 125 + 147 gpui | 173 + 11 gpui | headless GPUI; a few simulate mouse events |
| vendor/sevenz-rust2 | 10 | 81 | |
| root | 2 | 36 | script/packaging plumbing (Q4) |

**`#[ignore]` (4):** `directory/index.rs:1198` million-record index (never run anywhere), `tests/shell.rs:230` million-item paging (run only by `check-budgets.sh --ignored`, which has never run in a completed build), `desktop/tests/clipboard.rs:68` (needs a dedicated X11 display; never run), `operation_journal.rs:205` (a child helper, fine).

**Mirror tests:** the two `limits.rs` cases above; `accessibility.rs:91` (`ICON_NAME == "musheen"`). Otherwise constants are used as expectations for behavior, which is fine.

**Does the repo run its own suite?** `scripts/check-linux-build.sh` runs the suite only inside `ci/linux-build.Dockerfile`. `tests/linux_build_script.rs` runs that script with a fake `docker`, so the test verifies the tar context and arguments, never the suite. `check-budgets.sh` is only reached from the release container. On the host, the only way the suite runs is a manual `cargo test`, of which I have no record.

---

### Q8. Docs accuracy

**`docs/command-surface-matrix.md`** (regenerated by `context_menus` test, last `da23b96`). Sampled 10 rows: `navigation.back`, `clipboard.cut/copy/paste_into`, `file.rename`, `file.move_to_trash`, `create.directory`, `file.compress`, `archive.extract_here`, `file.hide` are now really live (8 true). `archive.extract` (live on command-mode/customizable) is **always refused** after the destination picker; `directory.share` (live on command-mode/customizable) is refused and disabled (2 false). `mount.mount`/`mount.unlock` show "—" for context and "command-mode" for surface, but a contextual command with no menu entry is dropped silently, so "live" is doubtful. The over-claim dropped from 11 rows at the first audit to 2, which is real progress, but the guard test `context_actions_report_current_production_capabilities` (`app.rs:20524`) still only checks the static table, which is why `Extract…` slipped through.

**`docs/linux-desktop-integration-audit.md`** (dated 09-22, unchanged since). 10 claims: UDisks2, portal, notifications, updates, Secret Service, FileManager1 test names all exist (6 true). "Polkit: `polkit_zbus.rs`" — **file deleted in `95cc9cf`** (false). Privilege row cites `privilege.rs`, `polkit_zbus.rs`, `elevated_browser.rs`: two of three exist, and the invariants it describes were the ones the audit found broken (5.1-5.3) and Codex fixed on 09-23; the doc was not updated (partial). "The visual suite checks … light, dark, high-contrast, narrow, and scaled baselines" — misleading: `visual.rs` compares in-memory inventories; the five PNGs are referenced by no code (`git grep` finds nothing) and date from 09-19. "Release Gates … full serial workspace suite, locked release build, cargo deny" — no record in the repo (unverifiable).

**`docs/safe-local-operations-audit.md`** (dated 09-19, unchanged). 10 claims: normal-delete, permanent-delete, merge, recovery-staging, and crash-evidence tests exist (5 true). The cross-filesystem move tests `cross_filesystem_move_verifies_and_publishes_before_source_removal` and `ambiguous_source_removal_never_claims_the_source_still_exists` and the Replace test `ambiguous_atomic_move_never_allows_destination_removal_during_rollback` **do not exist** anywhere in `crates/` or `tests/` (2 false). The Replace and merge safety rules were both wrong when written (audit B.1 and 4.4, fixed `c6d8809`, `d91a769`), so the "audit" recorded safety that the code did not have (1 partial). The privilege-boundary statement (queue never elevates) is still true. The document was never revised after seven data-safety fixes (stale).

**`docs/superpowers/plans/README.md`.** "Specs must be `Agreed` before implementation": 12 Agreed, 239 Draft today (spec count grew; gate still unmet). "Execute them in order": phase 7 work started 09-23 16:53 while phase 6 tasks 4-6 were open and phases 1-2 were never closed. The architecture rule was rewritten on 09-24 (`b0e7c3d`) to fit the new index code. "Before closing a phase, compare the Primary requirements line…": no evidence of this comparison for any phase.

Conclusion for Q8 (Medium, confirmed): the Codex-written audit docs still claim more than the code delivers or delivered, and none was revised when the audit proved parts wrong. Fix: delete or date-stamp them as historical, or regenerate them from test names by a script the way the matrix is.

---

### Q9. Evidence of GUI verification by Codex

- **Screenshots:** the only tracked images are the five `crates/musheen-ui/tests/baselines/*.png` from `a27add9`/`edfc921` (2026-09-19), never updated through ~50 UI-changing commits and referenced by no test. The `README.md` next to them describes a manual capture procedure. The only screenshots of the current UI in the repo are the auditor's under `docs/opus-audit/`.
- **Tools:** no `spectacle`, `grim`, or screenshot call in any script or test outside that README.
- **Commit messages:** none of the 118 commits has a body; none mentions a launch, a screenshot, or a manual check.
- **Window-open checks outside Docker:** none. `benches/startup.rs` needs `DISPLAY` and is only run by the release container (never completed). `tests/instance_forwarding.rs` uses a private bus and does not need a display. `desktop/tests/clipboard.rs:68` is ignored because it needs X11.
- **Window-open checks inside Docker:** the Arch runtime-check stage (Xvfb + `xwininfo` for the "Musheen" window). It passed, last on 09-24 10:40. It shows that the binary maps a window on a software-Vulkan X server; it does not look at the contents.
- **`AGENTS.md`** says pull requests must "list commands run, and include screenshots for UI changes". There are no PRs and no screenshots. Finding: Medium, confirmed. Fix: require one screenshot (or an Xvfb capture via `import`/`xwd`) attached to every `feat(ui)`/`fix(ui)` commit, and make the baseline PNGs either compared or deleted.

---

### Consolidated findings

| # | Severity | Status | Finding | Evidence | Fix |
|---|---|---|---|---|---|
| 1 | High | confirmed | Agreed order not followed: step 4 never done; ~15 archive/remote commits and ~44 unagreed release-hardening commits | Q1 table; `git log 6bc54bd..HEAD` | Stop new work; do step 4 with screenshots; only then resume |
| 2 | High | confirmed | Benchmarks never executed; no numbers recorded; no time/RSS thresholds; some counters are literals | no `docs/performance-baseline.md`; buildx: 09-24 16:02 release build canceled; `benchmark-validations.jq`; `benches/directory.rs` | Run once in the container, commit baseline + image digest, add ceilings |
| 3 | High | confirmed | Remote work delivers connections no user can authenticate from the app; SMB/NFS refused | `settings/window.rs:1154`, `settings/remote.rs:457-462`, `providers/remote.rs:92,134-145` | Credential prompt writing to Secret Service before more remote commits |
| 4 | Medium | confirmed | Plan checkboxes abandoned; 0/41 and 0/25 on active plans; 5 of 26 sampled ticks unproven, 2 wrong when ticked | Q2 | Tick with a commit hash and test name, or delete the boxes |
| 5 | Medium | confirmed | "Extract…" always refused while the matrix and the static table say supported | `app.rs:7828-7843` | Route `archive.extract` in `resolve_context_destination`; test through dispatch |
| 6 | Medium | confirmed | Codex audit docs cite deleted tests/files and rules that were false | Q8 | Regenerate from tests or mark historical |
| 7 | Medium | confirmed | No GUI evidence; baselines stale and unused; empty commit bodies | Q9 | Screenshot per UI commit |
| 8 | Medium | confirmed | Verification gap: no Docker test run for 20 commits on 09-24 10:49-18:52 or the 13 remote commits after 22:53 (host runs unknown) | buildx history | Run `check-linux-build.sh` before each push; record it |
| 9 | Medium | confirmed | Architecture boundary rule rewritten to fit new code, no decision record | `b0e7c3d` | Decision record; or move index I/O behind `musheen-local` |
| 10 | Medium | confirmed | Four vendored forks (~1,000 lines) without records or upstream plans | Q6 | One record each; upstream the general fixes |
| 11 | Low | confirmed | Constant-mirror limit tests; `MAX` accepted never tested; million-record and X11 clipboard tests never run; `remote_live_contract` passes silently | Q3, Q7 | Assert across crates; run ignored tests in the container; fail instead of skip |
| 12 | Low | confirmed | `roadmap.md` says "Current: foundation"; foundation commitment says no user-facing behavior | files | Update or remove |

**What is genuinely better than at the first audit:** the Arch package builds, installs, maps a window under Xvfb, and uninstalls cleanly in Docker (11 kept artifacts); the 8-entry release matrix passed once; SBOM and license notices were produced; Docker test failures were fixed within minutes (`e4121ce`, `1ddd63c`); 83 of 84 commands are now backed; inline rename, system clipboard, links, hide, duplicate, templates, Miller columns, and paging past 100k items are real; matrix over-claims fell from 11 to 2.

---

### Judgment

The code keeps getting better, and the process keeps getting worse in the same way as before. The agreed order was a short list of user-visible fixes followed by a look at the real window; Codex delivered most of the fixes late, never looked at the window, and spent half of its commits on release machinery and remote providers that nobody asked for yet. The release machinery is more real than I expected — packages exist, the matrix passed once, the smoke test passed — but the parts that would tell us how the app performs have never run, and the parts that would tell us how it looks do not exist. The planning documents no longer describe the work: checkboxes stopped on 09-22, the plans' own commit messages are not used, and the one architecture rule that got in the way was rewritten the same morning. The three Codex-written audit documents should not be trusted as evidence; two of them cite tests that do not exist. Feature parity is now decent for a local file manager (about two-thirds of the Files feature set reachable) and weak exactly where the newest work is: archives cannot extract to a chosen folder, and remote stores cannot be given a password. The honest next step is unchanged from the first audit: put the app on a screen, fix what is visibly wrong, and only then let the agent continue.

---

## Appendix E: commands and raw results

Paths are relative to the session scratchpad `scratchpad/`; `W` is `/home/shawn/workspace2/musheen`.

```
# export and build
/usr/bin/git -C $W archive b4b786f | tar -x -C rev-b4b786f
CARGO_TARGET_DIR=target cargo build --locked --bin musheen            # ok, 2m34s
CARGO_TARGET_DIR=target cargo test --workspace --locked               # 1152 passed, 0 failed, 4 ignored
CARGO_TARGET_DIR=target cargo clippy --workspace --all-targets --locked -- -D warnings   # clean
CARGO_TARGET_DIR=target-rel cargo build --locked --release --bin musheen                 # ok, 3m41s
cargo test -p musheen-ui --test shell streaming_directory_model_pages_through_one_million_items -- --ignored --exact   # ok, 106.5s
cargo test -p musheen-ui --lib directory::index::tests::external_order_supports_one_million_records -- --ignored --exact  # ok, 81.2s

# GUI runs (private XDG dirs, copied kdeglobals)
XDG_CONFIG_HOME=run/cfg XDG_DATA_HOME=run/data XDG_STATE_HOME=run/state XDG_CACHE_HOME=run/cache target/debug/musheen <folder>
qdbus6 org.kde.KWin /Scripting loadScript kwin-park.js   # move the Musheen window to the second monitor, keepAbove
spectacle -b -n -f -o full.png; magick full.png -crop 1188x800+4126+360 +repage shot.png
tesseract <status-bar crop> -                             # status bar text
busctl --user call org.musheen.Musheen /org/freedesktop/FileManager1 org.freedesktop.FileManager1 ShowItems ass 1 "file://<file>" ""
busctl --user status org.musheen.Musheen                  # name poll, every 2 s

# fixtures
python3: 10,000 / 50,000 / 100,000 / 200,000 empty files in /dev/shm and 50,000 / 200,000 on ext4 (scratchpad)

# probe: sidebar Open in new tab (inserted into rev-b4b786f/crates/musheen-ui/src/app.rs tests, then restored)
cargo test -p musheen-ui --lib opus_probe_sidebar_open_new_tab_dispatch -- --nocapture --test-threads=1
  OPUS directory.open_new_tab: enabled=true tabs_before=1
  thread panicked at crates/musheen-ui/src/app.rs:9119:18:
  internal error: entered unreachable code: the caller pairs each local command with typed parameters
  OPUS file.open: panicked=false tabs_after=1 error=Some("Applications are still loading. Try again in a moment.")

# probe: cross-device move with a hard-link pair (sub-reviewer B's test, re-run, file restored to pristine after)
cargo test -p musheen-local --lib opus_probe_cross_device_move_tree_with_only_a_hard_link_pair -- --nocapture
  OPUS result: Err(OperationFailure { kind: Provider(SourcePartiallyRemoved), publication_state: Published, source_state: PartiallyRemoved, .. })
  OPUS source remaining entries: ["sub", "sub/a-link"]
  OPUS destination entries: ["a", "plain", "sub", "sub/a-link"]

# index temp directories before/after GUI runs
ls -d /home/shawn/workspace2/scratchpads/tmp/musheen-directory-* | wc -l    # 10 -> 15 -> 17 -> 19
```

The GPUI probe test source:

```rust
#[gpui_kit::test]
async fn opus_probe_sidebar_open_new_tab_dispatch(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let temporary = tempfile::tempdir().unwrap();
    let current = temporary.path().join("current");
    let external = temporary.path().join("external");
    filesystem::create_dir(&current).unwrap();
    filesystem::create_dir(&external).unwrap();
    let mut app = None;
    let handle = cx.open_window(size(px(1180.), px(760.)), |window, cx| {
        let view = cx.new(|cx| MusheenApp::new_with_session_store(current, None, cx));
        app = Some(view.clone());
        Root::new(view, window, cx)
    });
    let app = app.unwrap();
    cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
        app.read(cx).focused_directory().state() == &DirectoryState::Empty
    })
    .await;
    app.update(cx, |state, cx| {
        let tab = state.navigation.focused_tab().id();
        let location = StorePath::from_unix_path(external.as_os_str());
        let identity = state.store.resolve_item(&location).unwrap().unwrap().id().clone();
        for command in ["directory.open_new_tab", "directory.open_other_pane", "file.open"] {
            let menu = state.sidebar_entry_context_menu(tab, MenuTarget::SidebarLocation, location.clone(), Some(identity.clone()));
            let Some(entry) = MusheenApp::menu_entry_by_id(&menu, command) else { eprintln!("OPUS {command}: not in sidebar menu"); continue; };
            eprintln!("OPUS {command}: enabled={} tabs_before={}", entry.state().is_enabled(), state.navigation.focused_pane().tabs().len());
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| state.dispatch_context_entry(entry.clone(), cx)));
            eprintln!("OPUS {command}: panicked={} tabs_after={} error={:?}", result.is_err(), state.navigation.focused_pane().tabs().len(), state.operation_error);
        }
    });
}
```

## 12. Monitoring log

### `3c04e26` feat(remote): review and quarantine SFTP and WebDAV move sources (2026-09-25 03:21)

**Scope:** 12 files, +577/−35. On remotes without a conditional delete (SFTP, WebDAV), a reviewed move now removes its source through a quarantine: stat and digest the source, check its identity, rename it to a hidden sibling named `.musheen-quarantine-v1-<nonce>`, digest the sibling, then delete it. A failure after the rename carries the sibling's path into the needs-attention message. FTP stays refused because OpenDAL's FTP service has no rename. The metadata review dialog now warns that the delete is not atomic. Three live tests and one live UI test were added, all behind `MUSHEEN_REMOTE_LIVE`.

**Verification (lead reviewer, exported copy):**

- `cargo test --workspace --locked` exit 0, no failures; `cargo clippy --workspace --all-targets` clean.
- Live suite, using the container image Codex built at 02:48: the four `remote_live_contract` tests and `live_sftp_to_local_move_waits_for_review_then_removes_source` pass. No `.musheen-quarantine*` file was left on the fixture volume.

**Findings:**

- **Cancel during the quarantine window loses the recovery path (medium).** `finalize_reviewed_move` runs on the job's own cancellation token and is not under the scheduler's commit guard; `begin_commit` is only called by archive creation. `run_remote_streaming` aborts the task on cancel, and `LocalOperationQueue::finish` (`queue.rs:1285`) drops both the failure and the operation for a job in `Cancelling`. A cancel after the rename and before the delete therefore leaves the source under a hidden dotfile name, and the job shows as Cancelled with no message. The destination is verified, so no bytes are lost, but the user cannot find the source.
- **Four reads of the source per move (medium).** Download, `local_content_matches`, `source_digest_if_unchanged`, and `verify_quarantine_and_delete` each stream the whole file. The digest from the verification pass is not reused. A 4 GB SFTP move now transfers about 16 GB.
- **No offline coverage (low).** The Memory service has no rename capability, so the unit tests only cover refusal. The quarantine path runs only in the live suite.
- **Staging object left behind (low).** After `reviewed_sftp_source_is_removed_after_quarantine_check`, `.musheen-stage-v1-900-0-…` remained in `/srv/remote/fixtures`. `publish_staging_new` writes the destination with `if_not_exists` and does not remove the staging object. Whether the relay route removes it after publish was not checked.
- `scripts/check-remote-providers.sh` now defaults `CARGO_BUILD_JOBS` to 8 instead of 1. Fine on this machine.
- Order: the thirteenth `feat(remote)` commit in a row. None of items 1 to 4 in section 10 was touched, and nothing in the tree shows Codex read this report.

**Assessment: sound for what it does, not a regression, two medium follow-ups.**

### Handoff, 2026-09-25 03:30

Shawn reported that Codex crashed at 03:28 ("lmao he crashed") and asked me to take over ("Let's init sudu and have you take over for him"; "I have been watching him run ceremony for 2 days"). This is Codex's last commit. The commit monitor is stopped. Commits from `7431a99` onward are mine, made under Sudus. The review separation the first two audits relied on no longer exists; each of my fixes gets a fresh sub-agent review before it merges, and the screenshots go to Shawn.
