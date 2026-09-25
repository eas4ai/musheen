Prefix: DEP

# Dependencies

Decided crate selections for musheen. Each requirement names one
dependency decision. Behavior using these crates lives in the owning
domain specs. Versions below are verified against crates.io as of
2026-09-19. `Cargo.toml` uses semver-compatible ranges, never exact
`=` pins, with reproducibility from the committed lockfile.

[DEP-001] The workspace declares native-theme and native-theme-gpui 0.5.x as the sole system-appearance source.
Falsifier: appearance code reads portal settings or desktop theme files directly instead of through native-theme.
Mechanism: dep-001
Rationale: 1.x mechanism: dependency check on Cargo.toml plus grep for direct portal or theme-file reads outside native-theme.
Status: Agreed 2026-09-18

[DEP-002] The workspace declares `xdg-mime` 0.4.x as the primary shared-MIME-info database and detector for names and content.
Falsifier: ordinary detection bypasses `xdg-mime` or uses an unrelated MIME database.
Mechanism: dependency and provider-boundary checks plus detection fixtures.
Status: Draft

[DEP-003] The workspace declares the freedesktop 0.0.x workspace crates for XDG directories and desktop detection (core), .desktop parsing and execution (apps), and icon theme lookup (icon).
Falsifier: a second .desktop parser or icon resolver is introduced alongside them.
Mechanism: dep-003
Rationale: 1.x mechanism: dependency check on Cargo.toml. Validation: freedesktop is young (~143 recent downloads at selection); it sits behind a thin app-owned trait so freedesktop-desktop-entry 0.8.x can replace it without touching callers.
Status: Agreed 2026-09-18

[DEP-004] The implementation resolves default applications with custom mimeapps.list code following the spec cascade order, using handlr parsing as reference.
Falsifier: default resolution delegates to a dormant crate instead of the custom resolver.
Mechanism: dep-004
Rationale: 1.x mechanism: review of the resolver module plus a fixture test over a layered mimeapps.list set.
Status: Agreed 2026-09-18

[DEP-005] The implementation uses `tree_magic_mini` only as a bounded content fallback when `xdg-mime` returns unknown or generic binary content, uses the system database, and never enables an embedded magic database.
Falsifier: fallback replaces a specific primary result, reads beyond its configured prefix, or embeds a magic database.
Mechanism: dependency feature check plus primary/fallback provider-spy tests.
Status: Draft

[DEP-006] The workspace declares `trash` 5.x for supported move-to-trash, list, restore, and purge operations. Irreversible deletion is allowed only through one app-owned permanent-delete boundary after confirmation.
Falsifier: normal delete bypasses the trash wrapper, or direct removal exists outside the permanent-delete boundary.
Mechanism: call-boundary check plus trash and permanent-delete fixtures.
Status: Draft

[DEP-007] The workspace declares nix 0.31.x for filesystem type and capacity detection, proc-mounts 0.3.x for the mount table, xattr for extended attributes, and reflink-copy 0.1.x for copy-on-write copies.
Falsifier: a second mount-table parser is introduced or raw libc replaces nix where nix offers the call.
Mechanism: dep-007
Rationale: 1.x mechanism: dependency check on Cargo.toml plus grep for direct libc calls covered by nix.
Status: Agreed 2026-09-18

[DEP-008] The workspace declares walkdir 2.x for local recursive traversal, rustix 1.x for syscall access, open 5.x for default-app launching, camino 1.x only for validated UTF-8 configuration and URI paths, wax 0.7.x for pattern matching, and the notify stable line for filesystem watching. `camino` never represents lossless local store paths.
Falsifier: a second crate is introduced in any of these six roles.
Mechanism: dep-008
Rationale: 1.x mechanism: dependency check on Cargo.toml. Validation: notify tracks stable 8.x until the 9.x prerelease finalizes.
Status: Agreed 2026-09-18

[DEP-009] The workspace declares `ashpd` 0.13.x with its client and backend features for portal consumption and optional FileChooser service, `zbus` 5.x for FileManager1 and UDisks2, and `notify-rust` 4.x for notifications.
Falsifier: a second portal wrapper, D-Bus binding, or notification client is introduced alongside them.
Mechanism: dependency check on Cargo.toml.
Status: Draft

[DEP-010] The workspace declares image 0.25.x and fast_image_resize 6.x for the isolated thumbnail-worker pipeline, with the freedesktop thumbnail cache layout, naming, mtime metadata, and fail records implemented as custom code.
Falsifier: thumbnails are written outside the spec cache layout or without mtime validation.
Mechanism: dep-010
Rationale: 1.x mechanism: fixture test over cache paths with stale-mtime and failure cases.
Status: Agreed 2026-09-18

[DEP-011] The workspace declares zip on its stable non-prerelease line with AES crypto for ZIP including encrypted archives, tar layered over flate2 and zstd for tarballs, and sevenz-rust2 0.23.x for 7z including encryption.
Falsifier: an archive format in scope is handled by hand-rolled codec code instead of these crates.
Mechanism: dep-011
Rationale: 1.x mechanism: dependency check on Cargo.toml plus round-trip fixture tests per format including encrypted variants. Validation: xz2 is dormant since 2022, so the xz backend choice falls to liblzma unless re-checked at integration time, and compress-tools with libarchive stays feature-gated for RAR and ISO only.
Status: Agreed 2026-09-18

[DEP-012] The workspace declares OpenDAL 0.59.x as the unified remote backend for FTP, FTPS, SFTP, WebDAV, and HTTP, declares pavao 0.3.x wrapped in spawn_blocking for SMB, serves NFS from kernel mounts with no userspace client, and keeps russh with russh-sftp as the fallback only if OpenDAL SFTP blocks.
Falsifier: a second remote framework is introduced for a protocol OpenDAL already covers, or SMB calls run on an async executor thread instead of spawn_blocking.
Mechanism: dep-012
Rationale: 1.x mechanism: dependency check on Cargo.toml plus review of the SMB call sites.
Status: Agreed 2026-09-18

[DEP-013] The implementation does not implement AFP and documents SMB as its substitute.
Falsifier: AFP wire-protocol code or an AFP dependency exists in the tree.
Mechanism: dep-013
Rationale: 1.x mechanism: grep check for AFP references outside this decision record.
Status: Agreed 2026-09-18

[DEP-014] Every dependency entry is compatible with musheen's GPL-3.0-or-later license unless you approve an exception in writing.
Falsifier: a license audit surfaces an incompatible license.
Mechanism: dep-014
Rationale: 1.x mechanism: license audit command pinned as the check mechanism at commitment time.
Status: Agreed 2026-09-18

[DEP-015] Cargo.lock is committed and the tree builds from the locked versions.
Falsifier: a clean checkout builds different dependency versions than the recorded lockfile.
Mechanism: dep-015
Rationale: 1.x mechanism: lockfile presence plus `cargo build --locked` locally and in the release-only workflow.
Status: Agreed 2026-09-18

[DEP-016] The workspace declares `portable-pty` 0.9.x for pseudoterminal process control and `alacritty_terminal` 0.26.x for terminal emulation; GPUI Kit owns rendering and input presentation.
Falsifier: terminal escape parsing or PTY job control is implemented by hand, or either dependency's UI becomes a second application shell.
Mechanism: dependency and feature checks plus terminal boundary tests.
Status: Draft

[DEP-017] The workspace declares `secret-service` 5.x with the selected async runtime and Rust crypto backend for Linux credential storage.
Falsifier: a second credential vault is introduced or secrets fall back to plain settings storage.
Mechanism: dependency feature check plus credential persistence tests.
Status: Draft

[DEP-018] The workspace declares Rust 1.95 as its minimum supported toolchain. Local release checks and release-only automation test that version as well as stable, matching the highest selected dependency MSRV at specification time.
Falsifier: the manifest advertises an older toolchain or locked dependencies fail to build on Rust 1.95.
Mechanism: manifest check plus locked local and release-only builds on 1.95 and stable.
Status: Draft

[DEP-019] The workspace declares `posix-acl` 1.2.x for local POSIX ACL reads and writes behind the metadata provider; xattr remains responsible only for extended attributes.
Falsifier: ACL bytes are parsed by hand or treated as ordinary user xattrs.
Mechanism: dependency-boundary check plus ACL round-trip fixtures.
Status: Draft

[DEP-020] The workspace declares `blake3` 1.x and `sha2` 0.11.x for BLAKE3 and SHA256 checksums through one streaming hash interface.
Falsifier: either checksum is implemented by hand or requires loading the whole file in memory.
Mechanism: dependency check plus known-answer and bounded-memory tests.
Status: Draft

[DEP-021] Musheen uses the Lucide 1.43 catalog already bundled by GPUI Kit 0.6.2 as its sole command and chrome icon family. The app registers only its selected icons through GPUI Kit's asset macro and does not add a second icon crate.
Falsifier: a command icon comes from another family or native builds embed the complete catalog without a measured need.
Mechanism: dependency, asset-registration, and icon-registry checks.
Status: Draft
