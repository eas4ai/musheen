# Recon

## Mechanisms carried from Cairn 1.x

The Cairn 1.x records were removed in the Sudus migration. These six
mechanism definitions are kept here so they can be declared again with
`sudus declare`. Each ran `results: per-requirement`.

| Name | Requirement | Command | Inputs |
|---|---|---|---|
| dep-001 | DEP-001 | `node scripts/check-dep-001.mjs` | `:(glob)**/Cargo.toml`, `Cargo.lock`, `scripts/check-dep-001.mjs`, `:(glob)**/*.rs` |
| dep-003 | DEP-003 | `node scripts/check-dep-003.mjs` | `:(glob)**/Cargo.toml`, `Cargo.lock`, `:(glob)**/*.rs`, `scripts/check-dep-003.mjs` |
| dep-007 | DEP-007 | `node scripts/check-dep-007.mjs` | `:(glob)**/Cargo.toml`, `Cargo.lock`, `:(glob)**/*.rs`, `scripts/check-dep-007.mjs` |
| dep-008 | DEP-008 | `node scripts/check-dep-008.mjs` | `:(glob)**/Cargo.toml`, `Cargo.lock`, `:(glob)**/*.rs`, `scripts/check-dep-008.mjs` |
| dep-014 | DEP-014 | `node scripts/check-dep-014.mjs` | `:(glob)**/Cargo.toml`, `Cargo.lock`, `deny.toml`, `scripts/check-dep-014.mjs`, `vendor/native-theme-gpui/README.md`, `vendor/native-theme-gpui/LICENSE-0BSD`, `vendor/native-theme-gpui/LICENSE-APACHE`, `vendor/native-theme-gpui/LICENSE-MIT` |
| dep-015 | DEP-015 | `node scripts/check-dep-015.mjs` | `:(glob)**/Cargo.toml`, `Cargo.lock`, `:(top)*[Cc]argo*`, `:(top)[rs]*`, `:(glob)**/*.rs`, `ci/dep-015.Dockerfile`, `vendor/native-theme-gpui`, `scripts/check-dep-015.mjs` |
