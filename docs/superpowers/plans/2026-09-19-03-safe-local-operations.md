# Safe Local Operations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver crash-safe, cancellable local file operations with conflicts, progress, recovery, and truthful durability.

**Architecture:** `musheen-ops` is a journaled state machine. It plans against a provider capability snapshot, stages reversible work, records durable transitions, and publishes immutable progress events. Only providers mutate storage; UI surfaces submit commands and observe jobs.

**Tech Stack:** Foundation stack plus trash 5.x, rustix/nix primitives, reflink-copy, xattr, posix-acl, and deterministic fault-injection fixtures.

---

**Primary requirements:** OPS-001–013, OPS-018–032; BROWSE-012; SEARCH-014,
SEARCH-019; DEP-006; LIMIT-009; UXF-008–013, UXF-016, UXF-021.

### Task 1: Define the operation state machine and scheduler

**Files:** Create `crates/musheen-ops/src/{lib,job,plan,state,scheduler,event}.rs`;
create `crates/musheen-ops/tests/{state_machine,scheduler}.rs`.

- [ ] Test every allowed transition from Planned through Running, Paused,
  Cancelling, Failed, Recoverable, Completed, and RolledBack; reject all other
  transitions and stale event generations.
- [ ] Test default ceilings of two data mutations and four metadata jobs, plus
  stricter provider limits and fair progress for queued jobs.
- [ ] Run the two tests; expect unresolved-type failures.
- [ ] Implement typed job IDs, immutable operation plans, cancellation tokens,
  progress units, and a scheduler that captures one `ResourceLimits` snapshot.
- [ ] Model undo as a newly validated inverse plan. Hide it when identity,
  capability, or current state makes that inverse unsafe.
- [ ] Run both tests with a deterministic clock and recording provider.
- [ ] Commit with `feat(ops): add operation state machine and scheduler`.

### Task 2: Add the durable journal and startup recovery

**Files:** Create `crates/musheen-ops/src/{journal,recovery,staging}.rs`;
create `crates/musheen-ops/tests/{journal,recovery_matrix}.rs`.

- [ ] Write crash-point tests before and after every journal append, fsync,
  rename, metadata application, destination publish, source removal, and staging
  cleanup. Each restart must choose resume, rollback, or ask—never guess.
- [ ] Run `cargo test -p musheen-ops --test recovery_matrix`; expect failure.
- [ ] Implement a versioned append-only journal with checksums, atomic snapshot
  compaction, directory fsync where supported, and quarantining of corrupt
  records. Record durability limitations in each job result.
- [ ] Keep staging on the destination filesystem when atomic publication is
  required; name and clean abandoned app-owned staging paths safely.
- [ ] Run the crash matrix on same-filesystem and cross-filesystem fixtures.
- [ ] Commit with `feat(ops): add journaled crash recovery`.

### Task 3: Implement copy and move without data loss

**Files:** Create `crates/musheen-ops/src/{copy,move,metadata_copy,verify}.rs`;
create `crates/musheen-ops/tests/{copy,move}.rs`.

- [ ] Test reflink, sparse, streamed fallback, symlink policy, hard-link
  preservation, xattrs, ACLs, mode/owner/time metadata, cross-device moves,
  special-file refusal, nested mounts, ENOSPC, permission loss, source changes,
  cancellation, and short writes.
- [ ] Run the targeted tests; expect failure.
- [ ] Implement capability-selected copy strategies and publish destinations
  only after data/metadata verification. Cross-device move is verified copy
  followed by source removal; cancellation never removes the only valid copy.
- [ ] Report skipped metadata explicitly and retain recovery instructions for
  any ambiguous outcome.
- [ ] Run tests on ext4-like, btrfs-like, FAT-like, and failure providers.
- [ ] Commit with `feat(ops): add verified copy and move`.

### Task 4: Implement create, rename, links, metadata, clipboard, and deletion

**Files:** Create `crates/musheen-ops/src/{create,rename,link,metadata,delete,batch_rename}.rs`,
`crates/musheen-desktop/src/clipboard.rs`; create
`crates/musheen-ops/tests/{create_rename,delete,links,metadata}.rs` and
`crates/musheen-desktop/tests/clipboard.rs`.

- [ ] Test invalid names, normalization collisions, case-only rename, cycles,
  atomic replacement, batch preflight, symlink targets, cross-device hard-link
  rejection, recursive file/directory mode separation, ACL changes, privilege
  needs, nested-mount scope, symlink-swap identity, trash restore, partial trash
  failure, permanent-delete confirmation tokens, and freedesktop copy/cut data.
- [ ] Run the targeted tests; expect failure.
- [ ] Implement all actions behind provider capability checks. Use `trash` for
  normal delete and one audited permanent-delete boundary for irreversible
  removal; never silently fall back from trash to permanent delete.
- [ ] Make batch rename preflight the entire mapping before the first mutation
  and journal temporary-name cycles.
- [ ] Submit permission/ownership changes only from a dirty, valid Properties
  plan after the user reviews its recursive scope. Make sidebar/content drops
  use the same copy/move queue and reject unsupported targets before drop.
- [ ] Run all tests including non-UTF-8 source and destination names.
- [ ] Commit with `feat(ops): add safe local mutation commands`.

### Task 5: Resolve conflicts and expose operation status

**Files:** Create `crates/musheen-ops/src/conflict.rs` and
`crates/musheen-ui/src/{status_center,dialogs/conflict.rs}`; create
`crates/musheen-ui/tests/operations.rs`.

- [x] Test replace, skip, keep-both, merge-folder, apply-to-all scoping,
  stale-destination revalidation, pause/resume/cancel, retry, partial success,
  dismissed-status persistence, and destructive confirmations that name command,
  scope, reversibility, and location without defaulting focus to destruction.
- [x] Run `cargo test -p musheen-ui --test operations`; expect failure.
- [x] Implement conflict decisions as journaled inputs tied to exact source and
  destination identities. Revalidate before applying a saved decision.
- [x] Render compact active progress plus a status center with completed,
  failed, recoverable, and needs-attention histories. Errors name affected
  paths and safe next actions.
- [x] Add a Trash surface that lists original location and deletion time,
  restores through conflict handling, and confirms Empty Trash by item count.
- [x] Run keyboard, accessibility, and restart-recovery UI tests.
- [x] Commit with `feat(ui): add conflict workflow and status center`.

### Task 6: Close the phase

- [ ] Run every operation test under fault injection and repeat the recovery
  matrix with forced process termination.
- [ ] Run format, Clippy, workspace tests, locked release build, and license
  audit.
- [ ] Inspect all deletion, overwrite, privilege, and path-conversion call sites
  manually; document why the last valid copy cannot be lost.
- [ ] Perform the rule 13 self-review, rerun affected checks, and commit with
  `test: close safe local operations evidence`.
