# Repository Guidelines

## Project Structure & Module Organization

Musheen is a Linux file manager written in Rust. Application code lives in
`src/`; keep modules grouped by the domains described in `docs/spec/` rather
than building one large UI module. Product requirements are split into focused
specifications such as `browse.md`, `ops.md`, `search.md`, and `ui.md`.
Implementation plans live in `docs/superpowers/plans/`. Vendored compatibility
code belongs in `vendor/`, static artwork belongs in `assets/`, and repository
checks live in `scripts/`.

## Build, Test, and Development Commands

- `cargo build --locked` builds with the committed dependency graph.
- `cargo run --locked` runs the current application locally on Linux.
- `cargo test --locked` runs the Rust test suite.
- `cargo fmt --all --check` checks Rust formatting.
- `cargo clippy --all-targets --all-features --locked -- -D warnings` runs the
  strict lint gate.
- `cargo deny check licenses` verifies dependency license compatibility.

Docker is optional for clean Linux build verification. Run at most one
container at a time. Keep development checks local. GitHub Actions may run
only for tagged or manually dispatched releases, never pushes or pull requests.

## Coding Style & Naming Conventions

Follow `rustfmt` defaults and the production rules in `BEST_PRACTICES.md`.
Use `snake_case` for modules and functions, `PascalCase` for types, and
`SCREAMING_SNAKE_CASE` for constants. Prefer small domain modules, explicit
error context, and lossless `Path`/`PathBuf` values for local filesystem paths.
Keep UI behavior aligned with native Linux themes; match the reference design's
layout and features, not its exact Windows styling.

## Testing Guidelines

Place unit tests beside the code under `#[cfg(test)]`; add integration tests in
`tests/` when behavior crosses modules. Name tests by observable behavior, for
example `copy_preserves_extended_attributes`. Cover success, cancellation,
permission errors, and filesystem capability differences. Never report a check
as passing unless it was run successfully.

## Commit & Pull Request Guidelines

Use the repository's short imperative convention, such as `docs: specify
terminal drawer` or `fix: preserve symlink target`. Keep commits focused. Pull
requests should explain user-visible behavior, name affected requirements,
list commands run, and include screenshots for UI changes.

## Agent-Specific Instructions

Cairn is disabled while it is being rebuilt. Do not run `cairn`, treat
`.cairn/` as historical data, and do not let Cairn records override the current
task. Preserve unrelated working-tree changes and use the local
`best-practices` skill for implementation and review work. For every Cargo
command on this machine, set `CARGO_TARGET_DIR` to
`/home/shawn/workspace2/scratchpads/musheen-target`. The `scratchpads/`
directory is only the parent for project-specific scratch space; never use it
as a Cargo target or shared build directory. Never use another project's
target directory, including `suprnova-target`.
