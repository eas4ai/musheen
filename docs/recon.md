# Recon

Written 2026-09-25 after the Sudus migration (Path B: a specification exists).
It covers the 457 commits since the newest Agreed date, 2026-09-18. Every
row is Exists, Documented, Contradicted, or Unverified, with a citation.
Two independent adversarial reviews already cover this ground in depth:
`docs/opus-audit.md` (2026-09-20 to 09-23) and `docs/opus-audit-2.md`
(2026-09-25). Rows cite them by section.

## Reading in one paragraph

Musheen is a working local file manager: it builds, 1,152 tests pass, and
browsing, navigation, sidebar, D-Bus forwarding, and 200,000-item folders
were verified on this desktop. It is not ready to ship. Four user-facing
defects are open (a crash from the sidebar menu, rubber-band selection
that ignores scroll, a cross-device move that leaves a hard-linked source
half-removed, and a trash path that copies across devices without
verification). Remote stores exist but cannot store a credential. The
process record is weak: empty commit bodies, stale plan checkboxes,
benchmarks never run, and no GUI verification outside these audits.

## History

| Claim | Status | Citation |
|---|---|---|
| 457 commits since 2026-09-18: 1 on 09-18, 200 on 09-19, 32, 30, 36, 73, 70 on 09-20 to 09-24, 15 on 09-25 | Exists | `git log --since=2026-09-18` |
| Every commit through `3c04e26` was authored by Codex (ChatGPT 5.6, then 6 from 09-23) under the eas4ai author | Documented | `docs/opus-audit.md` model-change note |
| All commit bodies since 09-23 are empty | Contradicted (AGENTS.md asked for screenshots on UI changes) | `docs/opus-audit-2.md` §4.6 |
| Codex stopped at 03:28 on 09-25; `7431a99` and `7b4a713` are the Sudus migration | Exists | `git log -3` |
| The agreed order (data safety, verbs, styling, screen check, archive and remote) was not followed; about 44 of 118 commits after 09-23 went to unagreed release hardening | Contradicted | `docs/opus-audit-2.md` §4.6 |

## Manifests

| Claim | Status | Citation |
|---|---|---|
| Workspace of 7 crates (`musheen-core`, `-desktop`, `-local`, `-ops`, `-test-support`, `-ui`, `-zstd-budget`) and 5 vendored forks (`gpui-component`, `gpui-pre`, `gpui-pre-linux`, `sevenz-rust2`, `native-theme-gpui`) | Exists | `Cargo.toml` `[workspace]` |
| GPUI Kit 0.6.4 | Exists | `Cargo.toml:55`, `Cargo.lock` |
| `overview.md` says GPUI Kit 0.6.2 | Contradicted by the manifest | `docs/spec/overview.md` "Technology choices" |
| Toolchain 1.95.0, MSRV 1.95 | Exists | `rust-toolchain.toml`, `Cargo.toml:31` |
| Only `native-theme-gpui` had a fork decision record; it was removed with the 1.x records and stays in history | Documented | `git show 7431a99^:docs/decisions/patch-native-theme-gpui-connector-for-gpui-kit-0-6-2.md` |
| The other four forks have no decision record | Contradicted (DEP policy expects one) | `docs/opus-audit-2.md` §10 item 10 |

## Entry points

| Claim | Status | Citation |
|---|---|---|
| One binary; `src/main.rs` is 65 lines and defers to `musheen-ui` | Exists | `src/main.rs` |
| Instance lock in `$XDG_RUNTIME_DIR/musheen/instance.lock` with a legacy fallback under `~/.config/musheen` | Exists | `src/instance.rs`; `docs/opus-audit-2.md` §3 |
| D-Bus `org.freedesktop.FileManager1` at `/org/freedesktop/FileManager1` plus the private name `org.musheen.Musheen` | Exists | `crates/musheen-desktop/src/file_manager1.rs:10-12` |
| A second launch forwards `ShowFolders` and navigates the focused tab rather than opening a window | Exists, verified in the GUI | `docs/opus-audit-2/second-launch-forwarded.png`; audit-2 §9 |
| `crates/musheen-ui/src/app.rs` is 27,264 lines | Exists | `wc -l` |

## Data

| Claim | Status | Citation |
|---|---|---|
| Session store (`session.json`), settings document, operation journal, and conflict journal are separate modules constructed with an explicit path | Exists | `crates/musheen-desktop/src/session.rs:27`, `settings.rs:106`, `operation_journal.rs:199`, `conflict_journal.rs` |
| The production directories those paths resolve to | Unverified in this pass | callers in `musheen-ui`; XDG lookups seen only in `apps/mod.rs`, `maintenance.rs`, `thumbnail.rs` |
| Directory index: `MSIDX001` records in a `tempfile` directory under `std::env::temp_dir()` after 4,096 resident items | Exists | `crates/musheen-ui/src/directory/index.rs:233`, `directory.rs:15` |
| That directory is tmpfs on this machine, and the index leaks on SIGTERM (10 to 19 directories in one run) | Contradicted (BROWSE-019 promises removal) | `docs/opus-audit-2.md` §5 |
| Trash goes through the `trash` crate; `supports_trash` reports support per path | Exists | `crates/musheen-local/src/mutation.rs:1584`, `:1594-1630` |
| On a volume without a usable trash directory the crate copies to the home trash without verification | Contradicted (OPS-008, OPS-021) | `docs/opus-audit-2.md` §4.4; `trash` crate `freedesktop.rs:577-608` |

## Tests

| Claim | Status | Citation |
|---|---|---|
| Workspace tests: 1,152 passed, 0 failed, 4 ignored at `b4b786f`; exit 0 with no failures at `3c04e26`; clippy clean at both | Exists | `docs/opus-audit-2.md` §3, §12 |
| Both million-item tests pass when run (106.5 s, 81.2 s) | Exists | audit-2 §3 |
| Live remote suite behind `MUSHEEN_REMOTE_LIVE` with a Docker fixture; passes at `3c04e26` | Exists | `scripts/check-remote-providers.sh`, `ci/remote-services.Dockerfile`; audit-2 §12 |
| Eight benchmark targets in `benches/`; no run output and no committed numbers exist | Contradicted (LIMIT budgets are unmeasured) | `benches/`; audit-2 §4.7 |
| No GUI verification in any script or test; the only window proof is an Xvfb `xwininfo` in the Arch package build | Contradicted (AGENTS.md) | audit-2 §4.6 |
| `docs/safe-local-operations-audit.md` (Codex, 09-19) cites 11 test names; 8 exist, 3 do not (`ambiguous_atomic_move_never_allows_destination_removal_during_rollback`, `ambiguous_source_removal_never_claims_the_source_still_exists`, `cross_filesystem_move_verifies_and_publishes_before_source_removal`) | Contradicted | grep `fn <name>` over `crates`, `tests`, `src` |
| `docs/linux-desktop-integration-audit.md` (Codex, 09-22) cites 5 test names; all 5 exist | Exists (audit-2 §10 overstated this) | same grep |

## CI and scripts

| Claim | Status | Citation |
|---|---|---|
| One workflow, `release.yml`, triggered by tags and manual dispatch; nothing runs tests on push | Documented gap | `.github/workflows/release.yml:3-7` |
| `ci/` holds Dockerfiles for the Arch package, DEP-015, Linux build, release, remote services, and SMB check | Exists | `ls ci` |
| 22 scripts: dependency checks (`check-dep-*.mjs`), release matrix, packaging, benchmarks, SBOM, remote and SMB fixtures | Exists | `ls scripts` |
| The six `check-dep-*.mjs` scripts were the Cairn 1.x mechanisms | Documented | section "Mechanisms carried from Cairn 1.x" below |

## Non-spec docs

| Claim | Status | Citation |
|---|---|---|
| Eight plans under `docs/superpowers/plans/`; checkboxes do not track the work (plan 01: 0 of 43 done though the foundation shipped; plans 04 and 05 all done since 09-21 and 09-22; plan 07: 0 of 41; 09-24 index plan: 0 of 25) | Contradicted as a status source | `grep -c '\[x\]'` per plan; `git log -1 -- <plan>` |
| `command-surface-matrix.md`, `custom-actions.md`, `themes.md` | Unverified against code in this pass | `docs/` |
| Two Opus audits with screenshots | Exists | `docs/opus-audit.md`, `docs/opus-audit-2.md`, `docs/opus-audit/`, `docs/opus-audit-2/` |

## Unresolved findings carried from `docs/opus-audit-2.md`

| Id | Finding | Severity | Spec | Code |
|---|---|---|---|---|
| 4.1 | Sidebar "Open in new tab" panics: `unreachable!` reached | High | UXF-012, BROWSE-002 | `crates/musheen-ui/src/app.rs:9119`, `dispatch_local_target_command` at `:8971` |
| 4.2 | Rubber-band selection ignores scroll: `logical_scroll_top()` is always zero for `uniform_list` | High | none (missing) | `app.rs:14416`, `rubber_band_indices` at `:482`, vendored `uniform_list.rs` |
| 4.3 | Cross-device move of a hard-link pair leaves the source half-removed; removal identity includes ctime | High | OPS-006 | `crates/musheen-local/src/operation.rs:1204-1212`, `:1052` |
| 4.4 | Trash copies across devices without verification when the volume root has no usable trash | High | OPS-008, OPS-021 | `crates/musheen-local/src/mutation.rs:1584-1630` |
| 4.5 | Remote not usable end to end: no credential field, "Extract…" refused, FTPS probe and browsing disagree | High | SYS-024, SYS-026, OPS-016 | audit-2 §4.5 |
| 4.6 | Process: order not followed, no GUI verification, empty commit bodies | Process | — | audit-2 §4.6 |
| 4.7 | Benchmarks never run | Medium | LIMIT | audit-2 §4.7 |
| 5.x | Index directory leaks on SIGTERM; index in tmpfs; FileManager1 log spam; two toplevels per instance; catalog observe, context-menu `stat`/`statfs`, and trash listing on the UI thread | Medium | BROWSE-019, UXF-007 | audit-2 §5 |
| 12 | Cancel during the remote quarantine window drops the recovery path; four wire reads per remote move | Medium | OPS-003, SYS-023 | audit-2 §12 |

## Blast radius of the defect commitment

The first commitment fixes 4.1 to 4.4. Its radius:

- **Modules.** `crates/musheen-ui/src/app.rs` (`dispatch_local_target_command`, `sidebar_entry_context_menu`, `rubber_band_indices`, `update_rubber_band`); `crates/musheen-local/src/operation.rs` (source removal identity); `crates/musheen-local/src/mutation.rs` (`supports_trash`, `move_to_trash`).
- **Tests.** `app.rs` test module (`rubber_band_geometry_*`, `keyboard_context_menu_*`); `crates/musheen-ops/tests/delete.rs`; `crates/musheen-local` move and trash tests; `tests/fault_matrix.rs`.
- **Spec sections.** browse.md "Tabs and panes", "Sidebar", "Directory views"; ops.md "Copy, move, and links", "Delete and rename", "Integrity and restart recovery"; ux.md "Focus and selection", "Direct manipulation", "Feedback and long-running work".

## Path B verdicts inside the radius

| Requirement | Verdict | Evidence |
|---|---|---|
| BROWSE-002 tab actions | Holds for the tab strip; the sidebar's "Open in new tab" route crashes | audit-2 §4.1 headless probe |
| BROWSE-005 open in other pane | Holds from the content view; absent from the sidebar menu | audit-2 §4.1 probe output |
| BROWSE-019 virtualized views, disk index | Holds for 10k to 200k items and memory; drifted on cleanup (leak on SIGTERM) and location (tmpfs) | audit-2 §3, §5, §7 |
| UXF-002 selection stays in one pane | Holds | audit-2 §3 |
| UXF-003 input paths share commands | Drifted: the sidebar path reaches an `unreachable!` arm | audit-2 §4.1 |
| UXF-012 menus offer only applicable commands | Drifted: the sidebar menu offers "Open in new tab" (crashes) and "Open" (launches an external program for a directory) | audit-2 §4.1 |
| Rubber-band selection | Missing: no requirement covers it; the behavior after scrolling is wrong | audit-2 §4.2; new block BROWSE-023 |
| OPS-006 move verifies before source removal | Holds for ordering; drifted for completeness: a hard-link pair leaves `SourcePartiallyRemoved` and retry cannot repair it | audit-2 §4.3 probe, run twice |
| OPS-008 trash or refuse | Drifted: `supports_trash` reports support where the crate would copy across devices | audit-2 §4.4 |
| OPS-021 metadata preserved or summarized before removal | Drifted through the trash path: mtime, hard links, and sparse layout are lost silently | audit-2 §4.4 |
| OPS-023 same-store hard links | Holds; the defect is the cross-store path | audit-2 Appendix B |
| OPS-030 revalidate identity before destructive work | Holds; the identity is stricter than the requirement needs (ctime), which is the cause of 4.3 | `operation.rs:1204-1212` |
| UXF-009, UXF-010 errors and retry | Drifted for 4.3: the error names the state, but retry cannot finish the move | audit-2 §4.3 |

## Mechanisms carried from Cairn 1.x

The Cairn 1.x records were removed in the Sudus migration. These six
mechanism definitions are kept here so they can be declared again with
`sudus declare`. Each ran `results: per-requirement`.

| Name | Requirement | Command | Inputs |
|---|---|---|---|
| dep-001 | DEP-001 | `node scripts/check-dep-001.mjs` | `:(glob)**/Cargo.toml`, `Cargo.lock`, `scripts/check-dep-001.mjs`, `:(glob)**/*.rs` |
| dep-003 | DEP-003 | `node scripts/check-dep-003.mjs` | `:(glob)**/Cargo.toml`, `Cargo.lock`, `:(glob)**/*.rs`, `scripts/check-dep-003.mjs` |
| dep-007 | DEP-007 | `node scripts/check-dep-007.mjs` | `:(glob)**/Cargo.toml`, `Cargo.lock`, `:(glob)**/*.rs`, `scripts/check-dep-007.mjs` |
| dep-008 | DEP-008 | `node scripts/check-dep-008.mjs` | `:(glob)**/Cargo.toml`, `Cargo.lock`, `:(glob)**/*.rs`, `scripts/check-dep-008.mjs` |
| dep-014 | DEP-014 | `node scripts/check-dep-014.mjs` | `:(glob)**/Cargo.toml`, `Cargo.lock`, `deny.toml`, `scripts/check-dep-014.mjs`, `vendor/native-theme-gpui/README.md`, `vendor/native-theme-gpui/LICENSE-0BSD`, `vendor/native-theme-gpui/LICENSE-APACHE`, `vendor/native-theme-gpui/LICENSE-MIT` |
| dep-015 | DEP-015 | `node scripts/check-dep-015.mjs` | `:(glob)**/Cargo.toml`, `Cargo.lock`, `:(top)*[Cc]argo*`, `:(top)[rs]*`, `:(glob)**/*.rs`, `ci/dep-015.Dockerfile`, `vendor/native-theme-gpui`, `scripts/check-dep-015.mjs` |

## Recon 2026-09-26: remote-usable superseded by remote-usable-extract

Scope: every commit since the newest Agreed date (2026-09-26), `dba48a3`
through `9070753`. The commits before `0c979d6` belong to finished
commitments (menu-composition-state, fallback-theme-follows-appearance-changes,
xattr-io-outside-catalog-queue), each with a done record. The radius is
archive extraction (OPS-033) and the remote requirements the successor
carries (SYS-024, SYS-026, SYS-031, SYS-032, DEP-022).

| Claim | Status | Citation |
|---|---|---|
| The extract engine publishes one new folder and refuses an existing destination: `Fail` returns `Conflict`, `Skip` skips the whole archive, `Replace` goes on. | Exists | `crates/musheen-desktop/src/archive/extract.rs:64-68` |
| Extract Here targets the archive's own folder, which always exists, so it always fails with "the archive destination already exists". No test ran Extract Here end to end before this commitment. | Exists (defect) | `crates/musheen-ui/src/app.rs` `build_extract_plan` (ExtractHere uses the parent); extract_to_ run of 2026-09-26 05:20 |
| "Extract here and Compress work." | Contradicted for Extract Here | `docs/opus-audit-2.md` 4.5 |
| Extract… reaches the destination chooser and, since `f83dfeb`, resolves a picked local folder without storage I/O and queues an extraction into that folder itself. | Exists | `ContextExtractDestinationResolver`, `build_extract_plan_into` in `crates/musheen-ui/src/app.rs` |
| The copy and move conflict dialog offers a per-conflict choice with an "apply to compatible remaining conflicts" switch. | Exists | `crates/musheen-ui/src/dialogs/conflict.rs:14-99` |
| An archive's entries can be listed without extracting it, which a collision preflight can use. | Exists | `crates/musheen-desktop/src/archive/store.rs:718` (OPS-015) |
| OPS-033 revised by the developer's ruling: a folder named after the archive, published in one step when absent and merged into when present, with Replace, Replace All, Skip or Skip All for each colliding item. | Documented | `docs/spec/ops.md` OPS-033; escalation `407ae943`, answer `3d44c334`; supersede `0e4e7cbc` |
| SYS-024, SYS-026, SYS-031 and SYS-032 are implemented in `f83dfeb`: shared `RemoteCredentials`, password field, session-only offer, Remove, browse-based Test connection with a named cause, SFTP on russh with agent, key file, stored key and `~/.ssh/config`. | Exists | `crates/musheen-desktop/src/remote/{credentials,sftp,connection}.rs`, `crates/musheen-ui/src/settings/remote.rs`, `crates/musheen-ui/src/providers/remote.rs` |
| The SFTP login tests pass except the RSA-agent one (8 of 9); the settings and parity tests pass except the offered-choice, SFTP editor and two Extract… tests (23 of 27). The fixes after that run are not yet built. | Unverified | run of 2026-09-26 05:20 |
| OpenDAL's HTTP service reads files but cannot list a folder, so browsing refuses HTTP connections (`BrowseRefusal::Protocol`). | Exists | `opendal-service-http-0.59.3/src/backend.rs:131` (no `list` capability); `ConnectionProfile::browse_refusal` |
| DEP-022 holds: musheen-desktop depends on ssh2-config 0.8.0. | Exists | `Cargo.lock`; receipt `196c5641` (pass) |

Path B verdicts inside the radius:

- OPS-033: Drifted from the engine it relies on; the developer ruled for the
  spec, revised before the successor start.
- OPS-015 (archive browsing without extraction): Holds for the collision
  preflight's needs; not otherwise verified here.
- OPS-016 and OPS-029 (path checks and archive limits): Still Observed; the
  merge keeps the engine's per-entry path checks and limits.
- SYS-024, SYS-026, SYS-031, SYS-032, DEP-022: Hold as agreed; carried
  unchanged into the successor.
