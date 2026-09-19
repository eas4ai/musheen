commitment: foundation
commit: 3f6a354
examined:
  - the exact candidate at 3f6a354 and commit range becceb5..3f6a354 in an isolated worktree
  - the foundation commitment, all six agreed requirements, their falsifiers, and the connector decision record
  - Cargo.toml, Cargo.lock, deny.toml, the vendored connector, and the locked-build Dockerfile
  - all six mechanism declarations and check scripts
  - the latest evidence receipts, input inventories, captured output, and the primary foundation review
  - offline clean runs and compiling falsifier probes for the non-Docker mechanisms
findings:
  - open: DEP-001 checks complete portal identifiers within one Rust file. A compiling probe split `org.freedesktop.portal.Settings.Read` and `org.freedesktop.appearance` across modules, invoked `gdbus` directly, and still passed DEP-001, so a direct portal settings read can escape the mechanism.
  - open: DEP-003 recognizes a hand-written Desktop Entry parser only when Rust source uses `.lines()` or `split_once('=')`. A compiling `[Desktop Entry]` parser using `split_terminator` and `splitn` still passed DEP-003.
  - open: DEP-003 has no source guard for a hand-written icon resolver. A compiling hicolor lookup built with `std::path` still passed DEP-003, so its icon-resolver check detects only the finite list of competing packages.
  - open: DEP-007 detects `libc::` spellings but not direct C bindings. A compiling `extern "C"` call to `statfs` still passed DEP-007 even though nix supplies that call.
  - open: DEP-007 misses mount-table paths assembled with ordinary path joins except for its special `mountinfo` case. A compiling parser for `Path::new("/proc").join("self").join("mounts")` still passed DEP-007.
  - open: DEP-008 treats any unwrapped Camino type use in a `config` or `uri` path as valid without checking the data's role or UTF-8 validation. A compiling `config.rs` probe converted a local store `Path` through `to_string_lossy()` into `Utf8PathBuf` and still passed DEP-008.

# Independent completion review

The candidate declares the required direct dependency families. Offline Cargo
metadata and the lockfile resolve the expected lines: native-theme and its GPUI
connector at 0.5.8, the freedesktop family at 0.0.3, nix at 0.31.3,
proc-mounts at 0.3.0, walkdir at 2.5.0, rustix at 1.1.5, open at 5.4.4,
camino at 1.2.6, wax at 0.7.0, notify at 8.2.0, xattr at 1.6.1, and
reflink-copy at 0.1.30. The vendored connector's runtime Rust source differs
from the cached 0.5.8 source only in the two recorded `tiles` assignments.
The omitted docs, examples, and matching example manifest entry do not enter
the runtime build.

Clean offline runs of DEP-001, DEP-003, DEP-007, DEP-008, and DEP-014 passed.
Each source-boundary finding above was then demonstrated with a tracked probe
that passed `cargo check --locked --offline` and also passed the requirement's
checker. The probes were removed, and all five clean checks passed again.

DEP-014's committed receipt covers the current manifests, lockfile, license
policy, vendored manifest, provenance, and license texts. A fresh offline
`cargo deny check licenses` through its mechanism also passed.

Per developer direction, Docker was not run during this review. The latest
DEP-015 receipt records a successful Linux Docker build of archived commit
166ba707 with `cargo build --locked`. Commit 3f6a354 adds only that receipt and
its captured output after 166ba707, so the declared build inputs remain the
ones that were built. The capture shows the expected locked direct versions
and a successful musheen build.

The six open mechanism gaps above prevent the evidence from excluding the
requirements' stated falsifiers. The commitment is not ready for Done.
