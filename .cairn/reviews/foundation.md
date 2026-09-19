commitment: foundation
commit: 58828b234ba9b59173d51c42e7dec40edffbe9a7
examined:
  - the six agreed dependency requirements and their falsifiers
  - every dependency mechanism, declaration, and latest evidence receipt
  - Cargo manifests, the lockfile, the license policy, and vendored connector metadata
  - repository automation for the required locked CI build
findings:
  - resolved: DEP-001 formerly allowed an aliased dark-light dependency; the mechanism now rejects known competing appearance providers by dependency key, package alias, and resolved lockfile package.
  - resolved: DEP-007 and DEP-008 formerly inspected only dependency keys; both mechanisms now reject competing packages declared under aliases.
  - resolved: DEP-015 formerly ran only a local locked build; the repository now has CI that runs `cargo build --locked`, and the mechanism verifies that command remains in the workflow.
  - resolved: The commitment formerly promised an out-of-scope advisory review; its exit evidence now names only the agreed license review.
  - resolved: The foundation exit evidence required the dependency set to pass advisory review without a mechanism, CI step, or passing audit; the commitment now limits its exit evidence to the agreed license review (independent 12d3043 1)
  - resolved: The DEP-001 source check formerly allowed a direct read of `/etc/gtk-3.0/settings.ini`; it now rejects GTK 3 and GTK 4 settings-file paths (independent 12d3043 2)
  - resolved: The DEP-007 source check formerly allowed a parser for `/proc/self/mountinfo`; it now rejects that Linux mount-table path along with the existing mount paths (independent 12d3043 3)
  - resolved: The DEP-008 mechanism formerly omitted Cargo.lock and allowed direct `globwalk`; it now verifies approved locked versions, rejects direct or aliased `globwalk`, and documents that alternate providers internal to approved dependencies are transitive implementation details rather than app-selected role providers (independent 12d3043 4)
  - resolved: The DEP-015 check formerly accepted a disabled workflow step; it now requires push and pull-request triggers, scopes the locked command to the `locked-build` job, rejects conditional execution and error suppression, and executes the same locked build for Cairn evidence (independent 12d3043 5)
  - resolved: The commitment formerly excluded all code changes despite the recorded vendored connector patch; its outcome now permits dependency-enablement changes while continuing to exclude user-facing behavior (independent 12d3043 6)
  - resolved: DEP-008 and the foundation exit evidence formerly left transitive helpers ambiguous; the exit evidence now distinguishes app-selected direct role providers from implementation details used only inside an approved dependency (independent d922070 1)
  - resolved: DEP-001 formerly allowed forbidden appearance identifiers assembled from source fragments; it now compares normalized source as well as contiguous spellings (independent d922070 2)
  - resolved: DEP-003 formerly checked only dependency metadata; it now rejects Rust source that combines Desktop Entry content with hand-written line and key/value parsing (independent d922070 3)
  - resolved: DEP-007 formerly parsed dependency-like TOML text; it now uses locked Cargo metadata for normal workspace dependencies and verifies approved resolved versions in Cargo.lock (independent d922070 4)
  - resolved: DEP-007 formerly allowed aliased `nix::libc` imports and fragmented mount-table paths; it now rejects libc imports or calls and compares normalized source for all forbidden mount paths (independent d922070 5)
  - resolved: DEP-008 formerly used a finite competitor blacklist; it now reads locked Cargo metadata and rejects every direct runtime package outside the foundation's reviewed dependency allowlist, including `ignore` under any alias (independent d922070 6)
  - resolved: DEP-008 formerly recognized only `camino::Utf8Path` spellings; it now rejects every `Utf8Path` or `Utf8PathBuf` type use outside configuration and URI modules regardless of import alias (independent d922070 7)
  - resolved: DEP-015 formerly checked only for trigger keys; it now requires unfiltered `push` and `pull_request` triggers and rejects nested branch or path filters (independent d922070 8)
  - open: The DEP-015 receipt proves a local locked build and committed CI configuration but records no GitHub Actions run identity, status, or log for the named candidate (independent d922070 9)

# Foundation completion review

The selected dependency families, resolved versions, license compatibility,
and current locked build are present. The review inspected the mechanisms for
manifest-alias and additional-appearance-provider bypasses, then compared the
commitment's exit claims with the automation actually in the repository. The
four findings above must be resolved before this commitment can be complete.

For the first resolution, the check passed on the repository and failed after
temporarily adding `appearance_probe = { package = "dark-light", ... }`; the
same check passed again after removing the probe.

For the second resolution, the clean checks passed, then DEP-007 rejected a
`procfs` package alias and DEP-008 rejected a `glob` package alias. Both checks
passed again after those temporary probes were removed.

For the third resolution, DEP-015 passed with the committed workflow, failed
when its command was temporarily changed to an unlocked build, and passed again
after restoring `cargo build --locked`.

For independent finding 2, DEP-001 passed on the clean tree and rejected a
temporary, compiling direct read of `/etc/gtk-3.0/settings.ini`. It passed again
after the probe was removed.

For independent finding 4, DEP-008 passed with the approved transitive graph,
failed after a temporary direct `globwalk` declaration, and passed again after
the declaration was removed.

For independent finding 5, DEP-015 passed with the committed workflow, failed
when the `locked-build` job was temporarily disabled with `if: ${{ false }}`,
and passed again after restoring unconditional execution.

For independent finding d922070 2, DEP-001 passed on the clean tree and rejected
a temporary, compiling `gsettings` command assembled with `concat!`. It passed
again after the probe was removed.

For independent finding d922070 3, DEP-003 passed on the clean tree and rejected
a temporary, compiling parser over `[Desktop Entry]` key/value lines. It passed
again after the parser was removed.

For independent finding d922070 4, DEP-007 passed on the clean tree and rejected
a temporary move of `nix` from normal dependencies into package metadata. It
passed again after restoring the dependency declaration.

For independent finding d922070 5, DEP-007 passed on the clean tree and rejected
a temporary, compiling `nix::libc as ffi` call plus a mountinfo path assembled
with `concat!`. It passed again after the probe was removed.

For independent finding d922070 6, DEP-008 passed with the approved direct
dependencies and rejected a temporary, locked, compiling `ignore` dependency.
It passed again after the dependency was removed and the lockfile regenerated.

For independent finding d922070 7, DEP-008 passed on the clean tree and rejected
a temporary, compiling `camino as utf8` local-path value in `src/main.rs`. It
passed again after the probe was removed.

For independent finding d922070 8, DEP-015 passed with unfiltered triggers,
failed when both triggers temporarily ignored every branch, and passed again
after the filters were removed.

For independent finding 3, DEP-007 passed on the clean tree and rejected a
temporary, compiling direct read of `/proc/self/mountinfo`. It passed again
after the probe was removed.
