commitment: foundation
commit: 12d304327a7ea34965b9bb53aac1857052fafea2
examined:
  - docs/commitments/foundation.md and DEP-001, DEP-003, DEP-007, DEP-008, DEP-014, and DEP-015 in docs/spec/deps.md at the reviewed commit
  - foundation history and diff from a42909a^ through 12d304327a7ea34965b9bb53aac1857052fafea2
  - Cargo.toml, Cargo.lock, the resolved dependency graph, deny.toml, and the vendored native-theme-gpui source and licenses
  - the native-theme-gpui compatibility decision and the vendored-source diff against crates.io native-theme-gpui 0.5.8
  - all six mechanism declarations, check scripts, evidence receipts, captured output, and fresh executions at the reviewed commit
  - .github/workflows/ci.yml, a fresh locked build, a fresh license audit, and a fresh advisory audit
  - safe violating examples for the DEP-001, DEP-007, DEP-008, and DEP-015 mechanisms, followed by restoration of the reviewed tree
findings:
  - open: The foundation exit evidence requires the dependency set to pass advisory review, but no mechanism, CI step, or recorded evidence runs an advisory check. A fresh `cargo deny check advisories` at the reviewed commit exits 1 for RUSTSEC-2024-0384, RUSTSEC-2024-0436, RUSTSEC-2025-0134, RUSTSEC-2026-0206, and RUSTSEC-2026-0192, so the stated exit condition is not met.
  - open: The DEP-001 source check does not enforce its direct-theme-file-read falsifier. Adding compiling Rust code that reads `/etc/gtk-3.0/settings.ini` directly still produces `cairn: DEP-001: pass` because the script checks only six fixed strings and does not recognize that theme file.
  - open: The DEP-007 source check does not enforce its mount-table-parser falsifier. Adding a compiling parser for `/proc/self/mountinfo` still produces `cairn: DEP-007: pass` because the script checks only `/proc/mounts` and `/proc/self/mounts`.
  - open: The DEP-008 mechanism cannot establish that no second crate is introduced in its six roles. It omits Cargo.lock from its inputs, uses a short fixed competitor list, and still passes after a direct `globwalk = "0.8"` dependency is added even though globwalk combines recursive traversal and glob matching. The reviewed lockfile already resolves globwalk 0.8.1 and notify 7.0.0 transitively alongside the selected walkdir, wax, and notify 8.2.0, but the evidence neither detects nor explains why those are not competing providers.
  - open: The DEP-015 evidence does not prove that the reviewed commit built in CI. The check only finds an unscoped `run: cargo build --locked` line and then builds locally; it still passes when that workflow step is disabled with `if: ${{ false }}`. The repository contains no CI result or attestation for commit 12d304327a7ea34965b9bb53aac1857052fafea2.
  - open: The commitment outcome says that no code change is in scope, but the reviewed range adds compiled vendored connector source and changes two production assignments under the native-theme-gpui compatibility decision. The patch matches the decision, but the commitment never reconciles that code work with its stated dependency-record-only scope.

# Review notes

The selected direct dependency versions match the six requirement texts. The locked build and license audit pass. The vendored connector matches the crates.io 0.5.8 source apart from the two decided field removals and the removal of package-only examples, metadata, and documentation assets.

The violating examples were made only in a detached temporary worktree. Each applicable example compiled. The reviewed candidate was restored and the detached worktree was clean before this report was written.
