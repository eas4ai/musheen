commitment: foundation
commit: b1557ab50d86b4830d91259ed92bf2d63ffb98bd
examined:
  - docs/commitments/foundation.md and the exact DEP-001, DEP-003, DEP-007, DEP-008, DEP-014, and DEP-015 texts in docs/spec/deps.md
  - every commit and changed path in a42909a^..b1557ab50d86b4830d91259ed92bf2d63ffb98bd
  - Cargo.toml, Cargo.lock, cargo metadata, and the resolved feature and package graph
  - all six mechanism declarations and scripts, including their declared input footprints
  - the latest evidence receipts and captured output for all six requirements
  - deny.toml, the live cargo-deny license result, and the vendored connector license and provenance files
  - .github/workflows/ci.yml and the live locked build
  - the native-theme-gpui patch decision and a diff against the cached upstream 0.5.8 crate
  - isolated falsifying examples for DEP-001, DEP-003, DEP-007, DEP-008, and DEP-015
findings:
  - open: The commitment says it contains no application behavior or code change, but the activation-to-HEAD range adds 7,340 lines of vendored Rust connector source and changes two upstream assignments before compiling that local patch through [patch.crates-io]; the patch decision explains the change but does not remove this direct conflict with the commitment's dependency-only scope.
  - open: The DEP-001 mechanism does not prove that native-theme is the sole appearance source because it searches Rust source for only six literal strings; an isolated tracked src/main.rs that directly read /etc/gtk-3.0/settings.ini still produced `cairn: DEP-001: pass`.
  - open: The DEP-003 mechanism uses a fixed seven-package denylist instead of proving that no second desktop parser or icon resolver exists; an isolated manifest that directly aliased the already locked rust-ini package as `desktop_parser` still produced `cairn: DEP-003: pass`.
  - open: The DEP-007 mechanism does not cover the normal Linux mountinfo table or raw extern-C calls; an isolated tracked src/main.rs that directly read /proc/self/mountinfo still produced `cairn: DEP-007: pass`, so the stated mount-table falsifier is not enforced.
  - open: The DEP-008 mechanism recognizes Camino use only when source contains the fully qualified `camino::Utf8Path` spelling; an isolated tracked src/main.rs imported Utf8PathBuf and used it for a local store path, yet still produced `cairn: DEP-008: pass`.
  - open: The DEP-015 mechanism proves only that the workflow text contains a `run: cargo build --locked` line and that a separate local build succeeds; an isolated workflow with `if: false` on the entire locked-build job still produced `cairn: DEP-015: pass`, so it does not prove that CI runs the locked build.

# Independent review

All six mechanisms pass on the reviewed tree. `cargo test --locked --all-targets`
also passes, but it runs zero application tests. `cargo fmt --all -- --check`
passes. The license audit reports `licenses ok`, and the current dependency graph
uses the required direct dependency families and locked versions.

The falsifying examples used isolated shared clones of the reviewed commit. Each
example changed only the file named in its finding and ran the owning mechanism.
The current repository was not changed by those demonstrations.

The five mechanism failures are independent of the current manifest being
well-formed. They show that later changes can violate the agreed falsifiers while
retaining passing evidence. The empty `reviewed:` lists in all six declarations
also contain no record that these boundaries were challenged before completion.
