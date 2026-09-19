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
  - open: The commitment promises an advisory review even though its included requirements and mechanisms cover licenses only; that exit statement cannot be proved by this commitment.

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
