commitment: foundation
commit: e85b21a
examined:
  - the six agreed dependency requirements and their falsifiers
  - every dependency mechanism, declaration, and latest evidence receipt
  - Cargo manifests, the lockfile, the license policy, and vendored connector metadata
  - repository automation for the required locked Linux build
findings:
  - resolved: DEP-001 formerly allowed an aliased dark-light dependency; the mechanism now rejects known competing appearance providers by dependency key, package alias, and resolved lockfile package.
  - resolved: DEP-007 and DEP-008 formerly inspected only dependency keys; both mechanisms now reject competing packages declared under aliases.
  - resolved: DEP-015 formerly ran only a host build; the mechanism now records `cargo build --locked` in a clean Linux container from the exact commit.
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
  - resolved: DEP-015 formerly relied on GitHub workflow text without a run attestation; by developer direction it now archives the exact commit and records a single clean Linux Docker build using `cargo build --locked`, with no GitHub CI during development (independent d922070 9)
  - resolved: DEP-001 formerly required the GTK directory and settings filename to be contiguous; it now rejects files containing both path components even when variables assemble them at runtime (independent 38c61f0 1)
  - resolved: DEP-003 formerly matched `DesktopEntry` case-sensitively; it now normalizes source case and separators before pairing Desktop Entry identifiers with line/key-value parsing (independent 38c61f0 2)
  - resolved: DEP-007 formerly required the mountinfo path components to be contiguous; it now rejects source containing both `/proc/self` and `mountinfo` components even when runtime code joins them (independent 38c61f0 3)
  - resolved: DEP-008 formerly allowed configuration modules to export Camino under another name; it now rejects Camino type aliases and renamed imports before applying the config/URI path allowance (independent 38c61f0 4)
  - resolved: DEP-015 formerly selected archive paths manually and omitted root `build.rs`; it now archives the full exact commit and copies that complete tracked tree into the Linux build container (independent 38c61f0 5)
  - resolved: DEP-001 formerly matched fragmented GTK paths one file at a time; it now aggregates normalized Rust source across the complete tree before checking required path components (independent 602a385 1)
  - resolved: DEP-003 formerly paired Desktop Entry markers and parsing operations per file; it now aggregates both signals across the complete Rust source tree (independent 602a385 2)
  - resolved: DEP-007 formerly allowed a hand-written mount-table parser to read `/etc/mtab`; the mechanism now rejects that legacy mount-table path (independent 602a385 3)
  - resolved: DEP-008 formerly allowed configuration and URI modules to export Camino inside wrapper structs or enums; the mechanism now rejects those wrapper types before applying the boundary allowance (independent 602a385 4)
  - resolved: DEP-015 formerly omitted repository Cargo configuration and Rust toolchain files from its declared inputs; the mechanism now invalidates evidence when either build-control input changes (independent 602a385 5)
  - resolved: DEP-001 formerly checked complete portal identifiers within one Rust file; it now aggregates the portal service, method, and appearance namespace signals across the full Rust source tree, so a direct portal settings read cannot escape by splitting constants across modules. (independent 3f6a354 1)
  - resolved: DEP-003 formerly recognized only `.lines()` or `split_once('=')`; it now also detects newline traversal with `split` or `split_terminator` and key/value parsing with `split` or `splitn`. (independent 3f6a354 2)
  - resolved: DEP-003 formerly checked only a finite list of competing icon packages; it now rejects Rust source that combines the hicolor theme with direct filesystem path traversal. (independent 3f6a354 3)
  - resolved: DEP-007 formerly detected `libc::` spellings but not direct C bindings; it now rejects raw C FFI declarations for the `statfs` and `statvfs` filesystem-capacity families that nix provides. (independent 3f6a354 4)
  - resolved: DEP-007 formerly missed proc mount-table paths assembled with ordinary path joins; it now rejects `/proc` or `/proc/self` roots joined to mount-table components. (independent 3f6a354 5)
  - resolved: DEP-008 formerly allowed config or URI modules to convert lossless local paths through `to_string_lossy()` into Camino values; it now rejects every lossy local-path conversion in a source file that uses Camino types. (independent 3f6a354 6)
  - resolved: DEP-001 now aggregates portal service, method, and appearance fragments across tracked Rust modules, so the prior split portal probe is rejected. (independent d4c184e 1)
  - resolved: DEP-003 now recognizes split and split_terminator line traversal plus split and splitn key/value parsing, so the prior alternate Desktop Entry parser is rejected. (independent d4c184e 2)
  - resolved: DEP-003 now rejects a hicolor resolver combined with direct filesystem path construction, so the prior hicolor path probe is rejected. (independent d4c184e 3)
  - resolved: DEP-007 now rejects raw C declarations for statfs, fstatfs, statvfs, and fstatvfs, so the prior raw statfs probe is rejected. (independent d4c184e 4)
  - resolved: DEP-007 now rejects proc mount-table paths built with ordinary path joins, so the prior joined `/proc/self/mounts` probe is rejected. (independent d4c184e 5)
  - resolved: DEP-008 now rejects `to_string_lossy` in a source file that uses Camino types, so the prior same-file lossy conversion is rejected. (independent d4c184e 6)
  - resolved: DEP-001 formerly checked the `kdeglobals` spelling per file; it now aggregates the `kde` and `globals` components across all tracked Rust modules. (independent d4c184e 7)
  - open: DEP-003 recognizes only a small set of line and key/value operations. A compiling `[Desktop Entry]` parser used `.lines()` with `find('=')` and slicing, and still passed DEP-003. (independent d4c184e 8)
  - open: DEP-003 recognizes a hand-written icon resolver only when the source also names hicolor. A compiling resolver walked `/usr/share/icons/Adwaita` with `std::fs::read_dir` and still passed DEP-003. (independent d4c184e 9)
  - open: DEP-007 lists only the global and self mount-table paths. A compiling parser read `/proc/1/mountinfo` directly and still passed DEP-007. (independent d4c184e 10)
  - open: DEP-007 rejects raw C bindings only for the statfs and statvfs families. A compiling probe called raw `stat`, which nix provides, and still passed DEP-007. (independent d4c184e 11)
  - open: DEP-008 treats a Camino type in a config module as valid unless that same file calls `to_string_lossy`. A compiling config probe converted a local `Path` through `path.display().to_string()` into `Utf8PathBuf` and still passed DEP-008. (independent d4c184e 12)

# Foundation completion review

The selected dependency families, resolved versions, license compatibility,
and current locked build are present. The review inspected the mechanisms for
manifest-alias and additional-appearance-provider bypasses, then compared the
commitment's exit claims with the automation actually in the repository. The
remaining findings above must be resolved before this commitment can be complete.

For the first resolution, the check passed on the repository and failed after
temporarily adding `appearance_probe = { package = "dark-light", ... }`; the
same check passed again after removing the probe.

For the second resolution, the clean checks passed, then DEP-007 rejected a
`procfs` package alias and DEP-008 rejected a `glob` package alias. Both checks
passed again after those temporary probes were removed.

For the third resolution, DEP-015 passed with the committed container build,
failed when its command was temporarily changed to an unlocked build, and
passed again after restoring `cargo build --locked`.

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

For independent finding d922070 9, the developer declined GitHub CI during
development and authorized one Linux Docker container at a time. DEP-015
archived the exact commit and completed `cargo build --locked` in that clean
container with one Cargo job; Cairn recorded the image build log and pass.

For independent finding 38c61f0 1, DEP-001 rejected a temporary compiling read
whose `gtk-3.0` directory and `settings.ini` filename were stored in separate
variables, then passed after the probe was removed.

For independent finding 38c61f0 2, DEP-003 rejected a temporary compiling
`parse_desktop_entry` function without title-cased markers, then passed after
the parser was removed.

For independent finding 38c61f0 3, DEP-007 rejected a temporary compiling read
that joined `/proc/self` and `mountinfo` variables at runtime, then passed after
the probe was removed.

For independent finding 38c61f0 4, the clean DEP-008 check passed and its new
alias guard matched the reviewer's `pub type ConfigPath = Utf8PathBuf` escape.

For independent finding 38c61f0 5, the checker now passes an unrestricted
`git archive` of the candidate to Docker, and the Dockerfile copies the entire
archive before running the locked build.

For independent finding 602a385 1, DEP-001 now aggregates all tracked Rust
source before checking GTK directory and settings-file components, so splitting
the constants across modules no longer changes the result.

For independent finding 602a385 2, DEP-003 now aggregates Desktop Entry domain
markers and line/key-value parsing operations across all tracked Rust modules.

For independent finding 602a385 3, DEP-007 passed on the clean tree and rejected
a temporary, compiling direct read of `/etc/mtab`. It passed again after the
probe was removed.

For independent finding 602a385 4, DEP-008 passed on the clean tree and rejected
a temporary `ConfigPath(Utf8PathBuf)` wrapper in an existing configuration
module. It passed again after the probe was removed. The same guard covers named
structs and enums.

For independent finding 602a385 5, DEP-015 uses pathspecs that include every
tracked `.cargo` file and any root `rust-toolchain` variant, so repository build
configuration changes make its Linux container evidence stale.

For independent finding 3f6a354 1, DEP-001 passed on the clean tree and rejected
a compiling probe that split the portal service, Settings/Read method, and
appearance namespace across two tracked Rust modules. It passed again after the
probe was removed.

For independent finding 3f6a354 2, DEP-003 passed on the clean tree and rejected
a compiling `[Desktop Entry]` parser that used `split_terminator` and `splitn`.
It passed again after the probe was removed.

For independent finding 3f6a354 3, DEP-003 passed on the clean tree and rejected
a compiling hicolor lookup that traversed icon directories with `std::path`.
It passed again after the probe was removed.

For independent finding 3f6a354 4, DEP-007 passed on the clean tree and rejected
a compiling raw C FFI declaration for `statfs`. It passed again after the probe
was removed. The same guard covers `statvfs` and both file-descriptor variants.

For independent finding 3f6a354 5, DEP-007 passed on the clean tree and rejected
a compiling parser for `Path::new("/proc").join("self").join("mounts")`. It
passed again after the probe was removed.

For independent finding 3f6a354 6, DEP-008 passed on the clean tree and rejected
a compiling `config.rs` probe that converted a local store `Path` through
`to_string_lossy()` into `Utf8PathBuf`. It passed again after removal.

For independent finding d4c184e 7, DEP-001 passed on the clean tree and rejected
a compiling probe that split `kde` and `globals` constants across two tracked
Rust modules. It passed again after the probe was removed.

For independent finding 3, DEP-007 passed on the clean tree and rejected a
temporary, compiling direct read of `/proc/self/mountinfo`. It passed again
after the probe was removed.
