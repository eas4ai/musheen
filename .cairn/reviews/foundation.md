commitment: foundation
commit: b99da33565b1042f50847c8f154124f837f7420c
examined:
  - the six agreed dependency requirements and their falsifiers
  - every dependency mechanism, declaration, and latest evidence receipt
  - Cargo manifests, the lockfile, the license policy, and vendored connector metadata
  - repository automation for the required locked CI build
findings:
  - resolved: DEP-001 formerly allowed an aliased dark-light dependency; the mechanism now rejects known competing appearance providers by dependency key, package alias, and resolved lockfile package.
  - open: DEP-007 and DEP-008 inspect dependency keys but not package aliases, so declarations such as mounts = { package = "procfs", version = "..." } or patterns = { package = "glob", version = "..." } bypass their competing-crate checks.
  - open: DEP-015 runs a locked local build, but the repository has no CI workflow to run it as the agreed mechanism requires.
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
