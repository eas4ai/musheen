# Shell baselines

These images record the Foundation shell after local enumeration settles. They
cover light, dark, high-contrast, 200% scale, and narrow-window layouts.

Build once with `cargo build --release --locked`, then run one instance at a
time from the workspace root. Theme previews affect only the Musheen process;
they do not change desktop settings.

```sh
MUSHEEN_THEME_PREVIEW=light target/release/musheen \
  crates/musheen-test-support/fixtures/shell-gallery

MUSHEEN_THEME_PREVIEW=dark GPUI_X11_SCALE_FACTOR=2 \
  MUSHEEN_PREVIEW_WINDOW_WIDTH=720 MUSHEEN_PREVIEW_WINDOW_HEIGHT=480 \
  target/release/musheen crates/musheen-test-support/fixtures/shell-gallery
```

Use `MUSHEEN_THEME_PREVIEW=high-contrast` for the accessibility baseline and
`MUSHEEN_PREVIEW_WINDOW_WIDTH=720` for the narrow baseline. Capture only after
the status bar reports `3 items`.
