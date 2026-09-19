commitment: foundation
commit: 12d304327a7ea34965b9bb53aac1857052fafea2
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
  - open: The DEP-001 source check does not enforce its direct-theme-file-read falsifier; Rust code that directly reads `/etc/gtk-3.0/settings.ini` still passes because the script checks only six fixed strings (independent 12d3043 2)
  - open: The DEP-007 source check does not enforce its mount-table-parser falsifier; a parser for `/proc/self/mountinfo` still passes because the script checks only `/proc/mounts` and `/proc/self/mounts` (independent 12d3043 3)
  - open: The DEP-008 mechanism cannot establish that no second crate is introduced in its six roles; it omits Cargo.lock, uses a short fixed competitor list, and passes with a direct `globwalk` dependency while the lockfile already contains transitive alternate provider versions (independent 12d3043 4)
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
