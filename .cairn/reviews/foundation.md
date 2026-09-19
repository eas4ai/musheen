commitment: foundation
commit: d922070e709c00f5688947ec554a34113e840627
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
  - open: DEP-008 and the foundation exit evidence do not authorize the checker's transitive-provider exception; the lockfile contains alternate transitive traversal, matching, and notification providers that the mechanism explicitly ignores (independent d922070 1)
  - open: DEP-001 can false-pass a compiled direct appearance bypass when `gsettings` and its schema are assembled from separate string fragments (independent d922070 2)
  - open: DEP-003 can false-pass a compiled hand-written parser for Desktop Entry key/value lines because it checks dependencies but no source boundary (independent d922070 3)
  - open: DEP-007 can false-pass with its four required crates moved to package metadata because it matches dependency-like text in any TOML section and omits Cargo.lock (independent d922070 4)
  - open: DEP-007 can false-pass compiled raw-libc and mount-table bypasses when imports are aliased and paths are assembled from string fragments (independent d922070 5)
  - open: DEP-008 can false-pass a compiled second traversal provider because its finite blacklist does not include the `ignore` crate (independent d922070 6)
  - open: DEP-008 can false-pass lossless local paths represented by Camino when the crate import is aliased (independent d922070 7)
  - open: DEP-015 can false-pass a workflow that excludes every branch under both declared triggers (independent d922070 8)
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

For independent finding 3, DEP-007 passed on the clean tree and rejected a
temporary, compiling direct read of `/proc/self/mountinfo`. It passed again
after the probe was removed.
