# Release Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove the completed file manager is bounded, recoverable, accessible, packageable, and reproducible on its supported Linux environments.

**Architecture:** Add no new product subsystem. This phase strengthens evidence at public boundaries: deterministic harnesses, benchmarks, fault injection, packaging metadata, migrations, supply-chain checks, and release gates run against the same production composition.

**Tech Stack:** Rust 1.95 and stable, sequential local Docker verification,
release-only GitHub Actions, Cargo/Clippy/rustfmt, cargo-deny, sanitizers where
supported, GPUI visual/a11y harnesses, D-Bus/portal fakes, and Linux
package/desktop metadata validators.

---

**Primary requirements:** DEP-018. This phase also supplies release-level
evidence for every earlier requirement and every limit.

### Task 1: Pin the supported build and supply-chain matrix

**Files:** Modify `rust-toolchain.toml`, workspace manifests, `Cargo.lock`,
`deny.toml`, and `ci/release.Dockerfile`; create
`.github/workflows/release.yml`, `scripts/verify-msrv.sh`,
`scripts/verify-dependencies.sh`, and `scripts/run-release-matrix.sh`.

- [ ] Make the local harness and release workflow fail if `rust-version`
  differs from 1.95, the lockfile changes during `--locked` builds, a
  dependency license is unapproved, an advisory is ignored without an
  expiring rationale, or duplicate role crates appear.
- [ ] Run the verification scripts before implementing harness fixes; capture
  the expected failing matrix entry.
- [ ] Run Rust 1.95 and stable, minimal and all features, debug and release, and
  supported desktop service matrix entries sequentially. Enforce a one-container
  lock and cache artifacts, not mutable dependency resolution.
- [ ] Configure GitHub Actions for version tags and manual release dispatch
  only. Do not run it for development pushes or pull requests.
- [ ] Generate a machine-readable SBOM and license notice from the lockfile.
- [ ] Run both scripts and the complete local release matrix.
- [ ] Commit with `build: enforce reproducible supported toolchains`.

### Task 2: Complete deterministic fault injection

**Files:** Expand `crates/musheen-test-support/src/{fault,clock,services,providers}.rs`;
create `tests/fault_matrix.rs`.

- [ ] Enumerate filesystem, journal, memory, worker, network, D-Bus, portal,
  credential, PTY, thumbnail, archive, and privilege boundaries. For each,
  inject failure before work, during partial work, after publication, and during
  recovery where meaningful.
- [ ] Run `cargo test --test fault_matrix`; first require it to report uncovered
  boundary IDs.
- [ ] Connect every production boundary to explicit injectable traits or fakes;
  keep production defaults unchanged and avoid test-only branches in policy code.
- [ ] Assert no failure loses the only good copy, exposes a secret, escapes a
  root, leaves unbounded staging, or claims success without evidence.
- [ ] Run the matrix repeatedly with deterministic seeds and process kills.
- [ ] Commit with `test: complete cross-system fault matrix`.

### Task 3: Enforce performance and resource budgets

**Files:** Create `benches/{directory,search,operations,thumbnail,archive,terminal}.rs`,
`scripts/check-budgets.sh`, and `docs/performance-baseline.md`.

- [ ] Benchmark startup, first directory page, one-million-item scrolling,
  search backpressure, large copy, thumbnail isolation, archive bombs, remote
  pools, and terminal flood. Record CPU, wall time, peak RSS, open descriptors,
  queued work, retained models, and temporary bytes.
- [ ] Make the budget script fail on every LIMIT-001–009 default or hard-max
  breach; distinguish correctness ceilings from advisory performance drift.
- [ ] Profile only measured regressions and fix the owning queue/model/provider
  rather than raising limits.
- [ ] Run release benchmarks in the pinned release container and commit the hardware/image
  identity with the baseline.
- [ ] Commit with `perf: enforce resource and responsiveness budgets`.

### Task 4: Lock visual, accessibility, and localization quality

**Files:** Expand `crates/musheen-ui/tests/{visual,accessibility}.rs` and locale
catalogs; create `scripts/check-ui-baselines.sh`.

- [ ] Cover all windows, views, drawers, dialogs, menus, status states, focus
  states, drag states, theme modes, reduced motion, RTL, pseudo-locale, narrow
  width, high contrast, and 100/200% scale.
- [ ] Fail on unnamed controls, unreachable commands, lost focus, clipped action
  text, color-only meaning, missing icon fallback, or unexplained baseline drift.
- [ ] Verify destructive and administrator actions remain visually and verbally
  distinct without depending on color.
- [ ] Review changed images manually and commit only intentional baselines.
- [ ] Commit with `test(ui): lock accessibility and visual baselines`.

### Task 5: Build installable desktop artifacts

**Files:** Create `packaging/org.musheen.Musheen.desktop`,
`packaging/org.musheen.Musheen.metainfo.xml`, D-Bus service and Polkit
policy files, icon install rules, and `scripts/package-smoke-test.sh`.

- [ ] Adopt `org.musheen.Musheen` as the application, desktop, AppStream, and
  private D-Bus identity; FileManager1 keeps its standard interface name.
- [ ] Validate desktop entry, AppStream metadata, icon sizes, D-Bus names,
  FileManager1 activation, portal feature isolation, broker permissions, and
  uninstall cleanup in a clean test image.
- [ ] Install the release binary under the unprivileged package root and verify
  no writable executable, setuid GUI, bundled credential, absolute build path,
  or unlicensed asset exists.
- [ ] Exercise native launch, Open With registration, default file
  manager activation, themes, portals, mounted devices, and update-check policy.
- [ ] Make package creation consume `assets/icons/musheen.svg` and generated
  raster sizes without introducing another app-icon design.
- [ ] Commit with `build: add validated Linux desktop packaging`.

### Task 6: Prove migrations, updates, and downgrade behavior

**Files:** Create `tests/migrations.rs`, signed-update fixtures, and
`scripts/release-upgrade-test.sh`; modify settings/catalog/journal migrations.

- [ ] Test every historical settings, session, catalog, credential-reference,
  operation-journal, thumbnail, and connection schema. Preserve unknown fields
  when safe and create recoverable backups before irreversible migration.
- [ ] Test disabled, delayed, offline, rollback, replay, wrong-channel, expired,
  tampered, and unsigned update metadata. Never present unsigned metadata as an
  available update.
- [ ] Test downgrade detection: refuse unsafe writes while offering export/reset
  instead of silently corrupting newer data.
- [ ] Run release-upgrade tests from the oldest supported fixture through the
  candidate version and one supported downgrade path.
- [ ] Commit with `test: prove data and update migrations`.

### Task 7: Execute the release gate

- [ ] Run `cargo fmt --check`.
- [ ] Run `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- [ ] Run `cargo test --workspace --all-features --locked` and every integration,
  fault, migration, visual, accessibility, and package test.
- [ ] Run `cargo build --release --locked`, `cargo deny check`, dependency-role
  audit, SBOM generation, secret scan, budget checks, and package smoke tests.
- [ ] Review requirement-to-test traceability for all CORE, DEP, BROWSE, OPS,
  SEARCH, CUSTOM, SYS, UXF, UIV, LIMIT, and ICON IDs. Reject missing, duplicate,
  always-pass, or stale mechanisms.
- [ ] Conduct a fresh adversarial review for destructive operations, privilege,
  archive traversal/bombs, remote identity, secret leakage, non-UTF-8 paths,
  crash recovery, and misleading UI states; resolve every finding separately.
- [ ] Reread BEST_PRACTICES.md rule 13. If any part is not production-ready,
  revise it and rerun its checks. Tag only the exact commit that passes the
  entire gate, then commit release evidence with `release: close musheen gate`.
