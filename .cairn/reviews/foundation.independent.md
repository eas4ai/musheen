commitment: foundation
commit: d922070e709c00f5688947ec554a34113e840627
examined:
  - The foundation commitment and DEP-001, DEP-003, DEP-007, DEP-008, DEP-014, and DEP-015 at the named commit.
  - The a42909a^..d922070e709c00f5688947ec554a34113e840627 commit list and changed-file range.
  - Cargo.toml, Cargo.lock, deny.toml, the vendored native-theme-gpui manifest and source patch, and the recorded compatibility decision.
  - All six mechanism declarations and scripts, their captured receipts, input records, stdout, and stderr.
  - The locked dependency graph, including duplicate and inverse trees for traversal, matching, and notification providers.
  - The GitHub Actions locked-build job and the DEP-015 evidence recorded by d922070e709c00f5688947ec554a34113e840627.
  - Baseline executions of every mechanism in an isolated archive of the named commit.
  - Isolated compiled falsifiers and matched failure controls for dependency declarations, provider boundaries, license rejection, and CI activation.
findings:
  - open: DEP-008 and the foundation exit evidence do not authorize the checker's transitive-provider exception. Cargo.lock contains globwalk 0.8.1, globset 0.4.20, ignore 0.4.33, and notify 7.0.0 alongside the selected wax, walkdir, and notify 8 providers, while check-dep-008.mjs explicitly ignores transitive competitors. Either the contract needs this narrower app-owned-provider rule or the committed lockfile does not meet the stated no-competing-crates evidence.
  - open: DEP-001 can false-pass a compiled direct appearance bypass. Replacing main with Command::new(concat!("g", "settings")) reading org.gnome.desktop.interface color-scheme passed DEP-001 and the locked build because the mechanism searches only contiguous forbidden spellings.
  - open: DEP-003 can false-pass a compiled second .desktop parser. A hand-written parser over Desktop Entry key/value lines passed DEP-003 and the locked build because the mechanism examines manifests and Cargo.lock but no source boundary.
  - open: DEP-007 can false-pass with none of its four required crates declared as dependencies. Moving nix, proc-mounts, xattr, and reflink-copy into package.metadata, regenerating Cargo.lock, and building with --locked passed both DEP-007 and DEP-015 because check-dep-007.mjs matches dependency-like text in any TOML section and does not examine Cargo.lock.
  - open: DEP-007 can false-pass compiled raw-libc and mount-table bypasses. `use nix::libc as ffi` followed by ffi::statvfs and read_to_string(concat!("/proc/self/", "mountinfo")) passed DEP-007 and the locked build because the mechanism depends on exact source spellings.
  - open: DEP-008 can false-pass a compiled second traversal provider. Adding ignore 0.4 as a direct dependency and calling ignore::WalkBuilder passed DEP-008 and the locked build because the competitor list is a finite package-name blacklist.
  - open: DEP-008 can false-pass lossless local paths represented by Camino. `use camino as utf8` followed by utf8::Utf8PathBuf in src/main.rs passed DEP-008 and the locked build because the source check recognizes only the unaliased camino::Utf8Path spelling.
  - open: DEP-015 can false-pass a workflow that never runs for branch pushes or pull requests. Adding branches-ignore: ["**"] under both declared triggers passed DEP-015 because the mechanism checks only that the trigger keys exist.
  - open: The DEP-015 receipt does not demonstrate the requirement's CI execution. Its command runs cargo build --locked locally and parses the workflow text; the repository records no GitHub Actions run identity, status, or log for the named candidate.

# Independent adversarial review

I extracted the named tree into a temporary directory and initialized an
isolated Git index so every script saw the exact tracked files without using
the developer's dirty working tree. I did not use the prior independent report
as review input.

All six mechanisms passed on the unmodified candidate. The locked build and
the cargo-deny license audit also passed. The vendored connector's Rust source
differs from the registry 0.5.8 source only at the two `tiles` assignments named
by the decision. The vendored package also trims examples, docs, proposals, and
the original manifest, and adds license files.

Matched controls failed as expected: literal `gsettings`, direct
`nix::libc::`, freedesktop-desktop-entry, jwalk, an AGPL-3.0-only local
dependency, and an explicit `if: false` on the CI job were all rejected. The
open items above are therefore false-pass gaps in otherwise executable checks,
not failures to run the tooling.
