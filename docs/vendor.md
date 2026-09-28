# Vendored crates

Musheen builds five crates from copies under `vendor/`, patched in through
`[patch.crates-io]` in the root `Cargo.toml`. Each section names the
published version the copy starts from, each change Musheen makes, and why.
The dep-023 check (DEP-023) keeps the headings in step with the vendored
versions. To move a crate to a new release, apply these changes to the new
published copy and update its heading.

## gpui-pre 0.3.7

The GPUI framework, at the version GPUI Kit 0.7 pins.

- `src/app/test_context.rs`: `TestAppContext::simulate_window_appearance`
  lets UI tests switch a test window between dark and light and run its
  appearance observers. The test window is private to the crate. Needed by
  the tests of UIV-014 (the fallback theme follows appearance changes).
- `src/elements/div.rs`, `src/window.rs`: `aria_has_popup` and
  `aria_disabled` expose a popup trigger and a disabled control to
  assistive technology, for the context menus of UXF-012.
- `src/window.rs`, `src/window/a11y.rs`, `src/window/a11y/debug.rs`:
  tests can build the AccessKit tree without a screen reader and read it
  as JSON, which now includes `has_popup` and `disabled`, so the menu and
  dialog accessibility tests can check what a screen reader would get.

## gpui-pre-linux 0.3.7

GPUI's Linux platform, at the version GPUI Kit 0.7 pins.

- `src/linux/wayland/window.rs`: when the compositor withholds frame
  callbacks while a window is covered, new application demand is served
  once without starting an idle loop, so a window that was covered does
  not stay unpainted when it is shown again.

## gpui-component 0.7.0

GPUI Kit's component layer, at the version GPUI Kit 0.7 pins.

- `src/menu/menu_item.rs`, `src/menu/popup_menu.rs`, `src/menu/mod.rs`:
  a menu item can say that it opens a submenu and whether that submenu is
  open; a custom menu row carries the accessible label, checked state and
  disabled explanation of the row that receives the click; and a menu can
  run right to left, which mirrors submenu placement, chevrons and arrow
  keys and passes to each child menu. Needed for Musheen's context menus
  (UXF-012) and for right-to-left locales.
- `src/menu/context_menu.rs`: when a context menu is dismissed, it drops
  its menu entity and the subscription at once, instead of keeping them
  until the next open. Upstream 0.7 now holds the shared state weakly,
  which fixes the leak for a window that closes with a menu open; Musheen
  also releases the menu on each dismissal.

## native-theme-gpui 0.5.8

The connector between native-theme and GPUI Kit (DEP-001).

- Trimmed to what Musheen builds: the showcase example and the proposals
  document are removed, and the crate's three license files (0BSD,
  Apache-2.0, MIT), which the published copy does not ship, are added.
- `Cargo.toml`: depends on GPUI 0.3.7, gpui-base 0.7.0 and gpui-component
  0.7.0, the versions GPUI Kit 0.7 uses, instead of the 0.6 line.
- `src/colors.rs`, `src/config.rs`: the `tiles` colour is not set, because
  gpui-component, in 0.6 and in 0.7, has no such field.
- `src/lib.rs`: `resolved_variant` returns the stored resolved theme for
  dark or light, so the Appearance settings can preview a mode and roll it
  back without rebuilding the theme from a preset.
- `src/icons.rs`: maps the three icons gpui-component 0.7 adds (`Ban`,
  `CircleAlert`, `RefreshCw`) to their Lucide, Material and freedesktop
  names.

## sevenz-rust2 0.23.0

The 7z reader and writer used for archives.

- `src/reader.rs`, `src/archive.rs`, `src/decoder.rs`, `src/error.rs`:
  archive metadata is decoded within a caller-supplied memory budget, with
  an error that names the allocation that did not fit; a lazily browsed
  archive reads only what a listing needs; the packed size of the block
  holding a file can be asked for; and the reader can visit every file
  once with its index, so extraction decodes each file once, in archive
  order (OPS-034).
- `src/encoder_options.rs`, `src/writer.rs`: each encoder reports its
  worst-case working memory, and the writer reserves its entry list
  before compressing, so archive creation can refuse a job that would pass
  its memory budget before it allocates.
- `src/encryption/password.rs`: a password's debug output hides its
  bytes.
- `rustfmt.toml`, `.github/workflows/rust.yml`, `Cargo.toml`: nightly-only
  format options removed so the workspace formats with stable rustfmt.
