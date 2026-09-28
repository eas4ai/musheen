# Vendored crates

Musheen builds five crates from copies under `vendor/`, patched in through
`[patch.crates-io]` in the root `Cargo.toml`. Each section names the
published version the copy starts from, then every file that differs from
that published crate, what changed and why. The dep-023 check (DEP-023)
keeps the headings in step with the vendored versions. To move a crate to
a new release, apply these changes to the new published copy, update its
heading, and compare the copy with the published `.crate` file again.

The `.cargo-ok` marker that Cargo writes when it unpacks a crate is not
part of any published crate, so it is not listed below.

## gpui-pre 0.3.7

The GPUI framework, at the version GPUI Kit 0.7 pins.

- `src/app/test_context.rs`: `TestAppContext::simulate_window_appearance`
  lets UI tests switch a test window between dark and light and run its
  appearance observers. The test window is private to the crate. Needed by
  the tests of UIV-014 (the fallback theme follows appearance changes).
- `src/elements/div.rs`: `aria_has_popup` and `aria_disabled` expose a
  popup trigger and a disabled control to assistive technology, for the
  context menus of UXF-012. The crate's own accessibility unit test also
  checks the disabled state.
- `src/window.rs`, `src/window/a11y.rs`, `src/window/a11y/debug.rs`:
  `activate_accessibility_for_test` lets tests build the AccessKit tree
  without a screen reader and read it as JSON, which now includes
  `has_popup` and `disabled`, so the menu and dialog accessibility tests
  can check what a screen reader would get.

## gpui-pre-linux 0.3.7

GPUI's Linux platform, at the version GPUI Kit 0.7 pins.

- `src/linux/wayland/window.rs`: when the compositor withholds frame
  callbacks while a window is covered, new application demand is served
  once without starting an idle loop (`FrameLoop::on_demand`), so a window
  that was covered does not stay unpainted when it is shown again. The
  window also returns a `frame_waker`, so demand from outside a frame can
  wake its frame loop. Unit tests (`frame_loop_tests`) cover the three
  wake cases.

## gpui-component 0.7.0

GPUI Kit's component layer, at the version GPUI Kit 0.7 pins.

- `src/menu/menu_item.rs`, `src/menu/mod.rs`: a menu row carries its
  accessibility role, description, checked state, disabled state, and
  whether it opens a submenu and that submenu is open. `mod.rs` exports
  `PopupMenuDirection`. Needed for Musheen's context menus (UXF-012,
  UIV-020).
- `src/menu/popup_menu.rs`:
  - A custom menu row carries the accessible label, checked state and
    disabled explanation of the row that receives the click, and a
    submenu row exposes its description, that it opens a menu, and
    whether it is open (UXF-012).
  - A menu can run right to left (`PopupMenuDirection`). This mirrors
    submenu placement, chevrons and arrow keys, and passes to each child
    menu, for right-to-left locales.
  - Keyboard, hover and Enter never select or activate a row that cannot
    be clicked. Upstream falls back to the first row, which can be a
    disabled row or a separator. Musheen keeps disabled rows visible with
    their reason (UXF-012), so they must not run. A menu with no
    clickable row has no selection.
  - A disabled submenu does not open, and entering a submenu selects its
    first clickable row.
  - A submenu opens on the side that has room, measured against the
    window's viewport and the submenu's own width, so a submenu near the
    window edge is not cut off. Upstream estimates the submenu's width
    from the parent menu's maximum width and compares it with the window
    bounds.
  - The submenu label and chevron have test ids, so UI tests can check
    which way the chevron points.
  - A unit test checks that a scrollable menu keeps its height limit,
    which Musheen sets from the window height.
- `src/menu/context_menu.rs`: when a context menu is dismissed, it drops
  its menu entity and the subscription at once, instead of keeping them
  until the next open. Upstream 0.7 now holds the shared state weakly,
  which fixes the leak for a window that closes with a menu open; Musheen
  also releases the menu on each dismissal. The unit test
  `dismiss_releases_the_menu_entity` checks it.

## native-theme-gpui 0.5.8

The connector between native-theme and GPUI Kit (DEP-001).

- Trimmed to what Musheen builds. Removed: the showcase example
  (`examples/showcase-gpui.rs` and its `[[example]]` entry in
  `Cargo.toml`), the screenshots in `docs/assets`, the proposals document
  (`proposals/README.md`), and the packaging files `Cargo.lock`,
  `Cargo.toml.orig` and `.cargo_vcs_info.json`. The crate's three license
  files (`LICENSE-0BSD`, `LICENSE-APACHE`, `LICENSE-MIT`), which the
  published copy does not ship, are added.
- `Cargo.toml`: depends on GPUI 0.3.7, gpui-base 0.7.0 and gpui-component
  0.7.0, the versions GPUI Kit 0.7 uses, instead of the 0.6 line; its
  tests use GPUI Kit 0.7.0.
- `src/colors.rs`, `src/config.rs`: the `tiles` colour is not set, because
  gpui-component, in 0.6 and in 0.7, has no such field.
- `src/lib.rs`: `resolved_variant` returns the stored resolved theme for
  dark or light, so the Appearance settings can preview a mode and roll it
  back without rebuilding the theme from a preset.
- `src/icons.rs`: maps the three icons gpui-component 0.7 adds (`Ban`,
  `CircleAlert`, `RefreshCw`). Freedesktop names exist for all three.
  Lucide and Material names are used only where native-theme 0.5.8
  bundles the file: Lucide `refresh-cw`, Material `error` and `refresh`.
  The others map to no icon, with the reason in the tests' allow lists.
  The tests' list of icon names covers the 104 variants of 0.7.

## sevenz-rust2 0.23.0

The 7z reader and writer used for archives. It is a workspace member, so
its tests run with the workspace tests.

- `src/reader.rs`, `src/archive.rs`, `src/decoder.rs`, `src/error.rs`,
  `src/lib.rs`: archive metadata is decoded within a caller-supplied
  memory budget (`ArchiveMemoryBudget`, `ArchiveMemoryLease`, exported
  from `lib.rs`), with an error that names the allocation that did not
  fit (`MemoryLimitExceeded`); the working memory of each LZMA, LZMA2 and
  PPMd decoder is computed and budgeted too; a lazily browsed archive
  reads only what a listing needs; the packed size of the block holding a
  file can be asked for; and the reader can visit every file once with
  its index, so extraction decodes each file once, in archive order
  (OPS-034).
- `src/encoder_options.rs`, `src/writer.rs`: each encoder reports its
  worst-case working memory, and the writer reserves its entry list
  before compressing, so archive creation can refuse a job that would
  pass its memory budget before it allocates.
- `src/encryption/password.rs`: a password's bytes are wiped from memory
  when it is dropped (`zeroize`), and its debug output hides them, so an
  archive password never stays in memory or reaches a log (OPS-017). A
  unit test checks the debug output.
- `Cargo.toml`:
  - adds the `zeroize` dependency for the password change;
  - drops flate2's `zlib-rs` feature, so Deflate in 7z archives uses the
    same miniz_oxide backend as ZIP, pinned in the root `Cargo.toml`,
    whose working memory the archive budget computes
    (`crates/musheen-desktop/src/archive/workspace.rs`);
  - the published `Cargo.lock` is removed, because a workspace member
    uses the workspace's lock file.
- `rustfmt.toml`: the nightly-only format options are removed, so the
  workspace formats with stable rustfmt.
- `.github/workflows/rust.yml`: trailing spaces are removed from blank
  lines. This came with the first patch and has no effect; Musheen does
  not run this workflow.
