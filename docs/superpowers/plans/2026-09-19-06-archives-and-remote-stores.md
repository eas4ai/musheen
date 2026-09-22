# Archives and Remote Stores Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add bounded archive browsing/creation/extraction and capability-honest FTP, FTPS, SFTP, WebDAV, HTTP, SMB, and mounted-NFS locations.

**Architecture:** Archives and remote services implement the same `Store` and operation contracts as local storage. Adapters translate native provider identity, paging, metadata, cancellation, and errors without pretending unsupported capabilities exist. All downloads/extractions stage and journal before publication.

**Tech Stack:** zip with AES, tar, flate2, zstd, sevenz-rust2, feature-gated libarchive bridge for RAR/ISO, OpenDAL, spawn-blocked pavao for SMB, kernel NFS mounts, and existing secret/journal infrastructure.

---

**Primary requirements:** OPS-014–017; SYS-020–026; DEP-011–013;
LIMIT-006, LIMIT-008.

### Task 1: Define safe archive paths and the archive store

**Files:** Create `crates/musheen-desktop/src/archive/{mod,path,store,format}.rs`;
create `crates/musheen-desktop/tests/archive_browse.rs` and archive fixtures.

- [x] Test absolute paths, `..`, Windows drive/UNC names, NUL, duplicate names,
  non-UTF-8 metadata, symlinks, hard links, encrypted headers, malformed central
  directories, 4,096-byte paths, and eight-level nested archives.
- [x] Run `cargo test -p musheen-desktop --test archive_browse`; expect failure.
- [x] Implement a provider-owned `ArchivePath` that cannot escape its virtual
  root. Add lazy paged enumeration and explicit read-only capabilities for ZIP,
  tar variants, 7z, and feature-gated RAR/ISO.
- [x] Require a password callback through secret-safe memory; errors and logs
  reveal neither password nor encrypted filenames beyond user-visible need.
- [x] Run the malformed corpus under time and allocation counters.
- [x] Commit with `feat(archive): add bounded archive store`.

### Task 2: Create and extract archives through the operation engine

**Files:** Create `crates/musheen-desktop/src/archive/{create,extract,budget}.rs`;
modify `crates/musheen-ops/src/{plan,event}.rs`; create
`crates/musheen-desktop/tests/archive_operations.rs` and
`crates/musheen-ops/tests/archive_recovery.rs`.

- [ ] Test round trips for ZIP/AES, tar.gz, tar.zst, and 7z/encryption; test
  conflict policy, cancellation, ENOSPC, bad password, symlink escape, special
  files, nested bombs, cleanup, and restart recovery.
- [ ] Independently trip entry-count, 20 GiB expansion, 1,000:1 ratio, nesting,
  path, 512 MiB memory, and temporary-space ceilings, then trip them combined.
- [ ] Run the archive operation test; expect failure.
- [ ] Implement codecs and budget accounting in `musheen-desktop` before any
  allocation/write. Submit typed compress/extract plans to `musheen-ops`, stage
  on the destination filesystem, verify normalized paths below the staging
  root, and publish through the existing journal.
- [ ] Run round-trip, bomb, crash-point, and cleanup fixtures.
- [ ] Commit with `feat(ops): add safe archive creation and extraction`.

### Task 3: Model remote connections and provider pools

**Files:** Create `crates/musheen-desktop/src/remote/{mod,connection,pool,error}.rs`;
create `crates/musheen-desktop/tests/remote_pool.rs`; modify Settings connection UI.

- [ ] Test validation and redaction for every protocol, Secret Service references,
  host-key/TLS decisions, proxy fields, cancellation, reconnect, saturated pool,
  15-second connect timeout, 60-second idle timeout, four requests per
  connection, and eight connections per provider.
- [ ] Run the pool test with a deterministic fake transport; expect failure.
- [ ] Implement versioned `ConnectionProfile` values without inline secrets and
  a fair cancellation-aware pool. Errors retain protocol/category and safe host
  context without credentials.
- [ ] Add connection-test and save flows; saving a failed test requires explicit
  confirmation and never weakens TLS/host-key policy silently.
- [ ] Run timeout, saturation, and log-redaction fixtures.
- [ ] Commit with `feat(remote): add secure connection profiles and pools`.

### Task 4: Implement OpenDAL-backed providers

**Files:** Create `crates/musheen-desktop/src/remote/{opendal_store,ftp,sftp,webdav,http}.rs`;
create `crates/musheen-desktop/tests/opendal_contract.rs`.

- [ ] Run the shared provider contract against FTP, FTPS, SFTP, WebDAV, and HTTP
  test services, including delayed pages, reconnects, unknown metadata, range
  reads, case policy, cancellation, and external replacement.
- [ ] Expect failures until adapters exist.
- [ ] Implement OpenDAL adapters with protocol-specific capability matrices and
  stable opaque IDs where available. HTTP is read-only unless the configured
  service proves mutation support. Use russh/russh-sftp only after a recorded
  OpenDAL SFTP blocker.
- [ ] Map provider errors into retryable, authentication, conflict, quota,
  permission, unsupported, and permanent categories.
- [ ] Run contract, operation, and hostile-network suites.
- [ ] Commit with `feat(remote): add OpenDAL storage providers`.

### Task 5: Add SMB and mounted NFS

**Files:** Create `crates/musheen-desktop/src/remote/{smb,nfs}.rs`;
create `crates/musheen-desktop/tests/{smb_contract,nfs_contract}.rs`.

- [ ] Test SMB blocking-call isolation, cancellation handoff, reconnect, shares,
  ACL/metadata gaps, case-insensitivity, name collisions, and server-side rename.
  Test that NFS locations arise only from kernel mounts and use local-provider
  semantics adjusted by detected mount capabilities.
- [ ] Run tests; expect failure.
- [ ] Wrap every pavao call in the dedicated blocking pool; no SMB function may
  execute on the async/UI executor. Reuse the local provider for mounted NFS and
  do not add a userspace NFS client.
- [ ] Document AFP as unsupported with SMB as the migration path; add a source
  audit that rejects AFP protocol dependencies or wire code.
- [ ] Run thread-recording, provider-contract, and disconnect tests.
- [ ] Commit with `feat(remote): add SMB and mounted NFS providers`.

### Task 6: Integrate remote operations and recovery

**Files:** Create `crates/musheen-ops/src/remote.rs`; modify operation planner,
status center, sidebar, and Properties; create
`crates/musheen-ops/tests/remote_operations.rs`.

- [ ] Test local↔remote and remote↔remote copy/move, server-side optimization,
  resumed transfer only when identity/range semantics prove safety, disconnect,
  credential expiry, destination replacement, unknown durability, and restart.
- [ ] Run the remote operation tests; expect failure.
- [ ] Plan from both providers' capabilities. Stage locally within temp budgets
  when direct transfer is impossible; report when atomic rename, fsync, sparse,
  ownership, ACL, xattr, or trash guarantees cannot be preserved.
- [ ] Keep offline locations visible with reconnect/remove actions and never
  present stale cached listings as live.
- [ ] Run crash and reconnect matrices for every protocol.
- [ ] Commit with `feat(ops): integrate recoverable remote transfers`.

### Task 7: Close the phase

- [ ] Run archive fuzz corpus, all provider contract suites, hostile networks,
  operation recovery, secret scans, format, Clippy, workspace tests, locked
  release build, and license audit.
- [ ] Verify memory/temp/timeout/pool counters against LIMIT-006 and LIMIT-008.
- [ ] Perform the rule 13 self-review and commit with
  `test: close archive and remote provider evidence`.
