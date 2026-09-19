commitment: foundation
commit: a849855cbb8d2ba74231f0c6d104d9dadb9a7cef
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
  - open: The DEP-015 evidence does not prove that CI runs the locked build; its unscoped text check still passes when the workflow step is disabled, and no CI attestation exists for the reviewed commit (independent 12d3043 5)
  - open: The commitment outcome says no code change is in scope, but the reviewed range adds and modifies compiled vendored connector source without reconciling that work with its dependency-record-only scope (independent 12d3043 6)

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

For independent finding 3, DEP-007 passed on the clean tree and rejected a
temporary, compiling direct read of `/proc/self/mountinfo`. It passed again
after the probe was removed.
