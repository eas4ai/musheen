# Custom themes

Settings → Appearance accepts a theme document in **Custom theme tokens**.
Choose **Start a custom theme** for an editable example, change its colors,
then choose **Import and preview**. **Copy theme document** exports the current
valid preview to the clipboard. **Apply** saves it; **Cancel** restores the
appearance captured when Settings opened. **Use native tokens** removes the
custom palette while keeping the selected appearance mode. Reset Appearance
also returns the mode and motion preference to their defaults.

The appearance mode remains a separate setting: follow the system, light,
dark, or high contrast. High contrast takes precedence over custom colors and
retains the native accessible palette with visible boundaries and focus rings.
Reduced motion requested by the desktop remains enabled.
While follow-system mode is active, desktop theme changes apply without a
restart and keep the user's saved semantic color overrides.

Theme documents have version `1` and thirteen required opaque `#RRGGBB`
colors. The semantic colors also supply component interaction states, chrome,
scrollbars, menus, inputs, and buttons. Icon families, fonts, focus visibility,
and accessibility preferences cannot be changed by an imported palette.

```json
{
  "version": 1,
  "tokens": {
    "background": "#ffffff",
    "foreground": "#111111",
    "primary": "#222222",
    "primary_foreground": "#ffffff",
    "secondary": "#eeeeee",
    "secondary_foreground": "#111111",
    "muted_foreground": "#444444",
    "border": "#555555",
    "ring": "#777777",
    "danger": "#330000",
    "danger_foreground": "#ffffff",
    "warning": "#332200",
    "warning_foreground": "#ffffff"
  }
}
```

Imports are limited to 8 KiB. Unknown fields, missing colors, unsupported
versions, transparency, and insufficient contrast are rejected. Text pairs
must meet 4.5:1 contrast; boundaries and focus indicators must meet 3:1.
An invalid import keeps the last valid preview and persisted settings, and
prevents Apply until a valid import, reset, or cancellation clears the error.
Saved JSON is normalized to one line for the versioned settings store.
