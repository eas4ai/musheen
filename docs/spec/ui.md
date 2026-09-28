Prefix: UIV

# Visual language

Musheen follows the Files layout and feature hierarchy: an integrated tab
strip, navigation row, command bar, sectioned sidebar, primary content
surface, optional info pane, and compact status bar. The reference does
not set pixel styling. `native-theme` supplies the Linux appearance, with
user overrides applied through GPUI Kit. Windows chrome and Mica are not
copied.

The durable structural reference is this layout, not the pixels of the
supplied Files screenshot:

```text
+ tab strip -------------------------------------------------------+
| navigation / breadcrumbs / search / view controls               |
| command bar                                                     |
+ sidebar ----+ primary pane --------+ optional second/info pane --+
| places      | directory content    | directory content or info   |
| pins        |                      |                             |
| mounts      |                      |                             |
+-------------+ terminal drawer (optional, resizable) -------------+
| status bar / operation status center                            |
+-----------------------------------------------------------------+
```

Review (2026-09-19): compared the supplied Files reference against every
shell region and interactive state. The requirements separate stable
layout hierarchy from theme-dependent appearance and cover scale, contrast,
reduced motion, narrow windows, and empty or failed content.

## Shell composition

[UIV-001] Each window arranges the tab strip above navigation, places the sidebar beside the active content surface, and places the status bar below content.
Falsifier: a normal desktop window presents those primary regions in a different hierarchy.
Mechanism: shell hierarchy assertions plus repo-owned screenshots at standard widths.
Status: Draft

[UIV-002] The navigation row groups back, forward, parent, refresh, breadcrumbs or omnibar content, search, and view controls by task.
Falsifier: a listed navigation control appears in an unrelated content or mutation group.
Mechanism: component-tree assertion and shell screenshot review.
Status: Draft

[UIV-003] The command bar presents frequent file actions before view, sort, group, and pane controls.
Falsifier: view configuration interrupts the primary mutation-action group.
Mechanism: command-bar order test for normal and compact widths.
Status: Draft

[UIV-004] The sidebar renders labeled, collapsible sections for places, pinned locations, mounts, remote locations, network, and tags.
Falsifier: items from distinct sections form an unlabeled continuous list.
Mechanism: sidebar component test with every section populated.
Status: Draft

[UIV-005] The content uses the primary surface token; sidebar, toolbar, dialogs, and status regions use their named chrome or overlay tokens and theme-supplied separators.
Falsifier: a shell region hardcodes emphasis outside its assigned token role.
Mechanism: component token-role assertions plus theme screenshots.
Status: Draft

## Components and states

[UIV-006] Interactive components define default, hover, pressed, focused, selected, disabled, drag-target, and error states when those states apply.
Falsifier: an applicable state has no visual distinction from default.
Mechanism: component-state gallery screenshots in light, dark, and high-contrast themes.
Status: Draft

[UIV-007] Selection uses the theme accent with a visible boundary that remains distinct from hover and keyboard focus. The accent is the desktop's accent colour; on Plasma, when the colour scheme sets no accent colour, it is the scheme's selection background, as Plasma uses it. Selected items in every layout, the selected sidebar entry, the current tab, the current breadcrumb, and menu highlights all use it.
Falsifier: a selected item cannot be distinguished from hover or focus alone; or one of those surfaces shows a colour other than the desktop's accent, such as a built-in blue while Plasma's scheme sets its selection colour.
Mechanism: uiv-007
Rationale: Shawn's report of 2026-09-27 (item linux-native-look): the context menu used his scheme's purple while the sidebar, tabs and path row used Breeze blue.
Status: Agreed 2026-09-28

[UIV-008] All command and chrome icons use the Lucide family defined in `icons.md`. Native content icons and reviewed provider marks stay within the role boundaries defined there. Unfamiliar and destructive symbols have accessible labels or tooltips.
Falsifier: equivalent actions use conflicting icon styles, an exception appears outside its allowed role, or an ambiguous icon has no text alternative.
Mechanism: icon-registry, role-boundary, and accessible-name tests.
Status: Draft

[UIV-009] Rounded corners, border weight, spacing, type scale, and elevation come from shared design tokens.
Falsifier: a shell component hardcodes one of those values outside the token layer.
Mechanism: source scan plus token-coverage test for shell components.
Status: Draft

[UIV-010] Directory layouts share selection, focus, label, thumbnail, and metadata components even when their spatial arrangement differs.
Falsifier: switching layout changes the visual meaning of a shared item state.
Mechanism: cross-layout state gallery and component identity assertions.
Status: Draft

## Responsive layout

[UIV-011] At narrow supported widths from 720 through 959 logical pixels, the command bar moves lower-priority actions into an overflow menu without hiding the active operation or navigation state.
Falsifier: resizing makes an active or required command unreachable.
Mechanism: width-sweep layout test with every command enabled.
Status: Draft

[UIV-012] The sidebar and info pane can collapse independently while the content surface retains a visible way to restore either pane.
Falsifier: a collapsed pane cannot be restored without reopening the window.
Mechanism: responsive interaction test across minimum, medium, and wide widths.
Status: Draft

[UIV-013] Dual-pane mode is available at 960 logical pixels or wider and maintains a visible divider and distinct focus treatment for the active pane. Below 960, the second pane remains in the model but is hidden behind a pane switcher.
Falsifier: the user cannot identify or reach the focused pane, or resize the pane boundary where both panes are visible.
Mechanism: dual-pane screenshots, pane-switcher tests, and divider interaction tests.
Status: Draft

## Theme, motion, and content states

[UIV-014] The theme layer derives every available system appearance input from the native-theme bridge and uses named fallback tokens for values it does not provide. When the bridge cannot read the system theme, the fallback preset follows the window's dark or light appearance.
Falsifier: a component copies a Files color or Windows material instead of using a theme token, or the fallback installs a variant the window appearance did not request.
Mechanism: uiv-014
Rationale: 1.x mechanism: theme source audit plus token-resolution tests with complete and partial native themes; docs/opus-audit-2.md A-F8: the fallback applied Adwaita dark and then light, so light always won.
Status: Agreed 2026-09-25

[UIV-015] Text, icons, focus indicators, and essential boundaries meet WCAG AA contrast in light, dark, and high-contrast themes.
Falsifier: any listed essential element falls below its applicable AA ratio.
Mechanism: automated contrast audit over the component-state gallery.
Status: Draft

[UIV-016] Motion communicates spatial change or operation state and becomes instant or minimal when the system requests reduced motion.
Falsifier: decorative or large movement continues unchanged under reduced motion.
Mechanism: animation-duration audit and reduced-motion interaction tests.
Status: Draft

[UIV-017] Directory and pane surfaces define loading, empty, partial, offline, and error presentations that preserve available navigation and recovery actions.
Falsifier: a non-success state replaces the surface with an unactionable blank region.
Mechanism: state screenshots and action-presence tests for every listed presentation.
Status: Draft

[UIV-018] The visual regression suite captures the shell and core component states at standard and narrow widths in light, dark, and high-contrast themes.
Falsifier: a shell region or applicable component state has no maintained baseline.
Mechanism: baseline inventory check plus deterministic screenshot test run.
Status: Draft

## Terminal drawer

[UIV-019] The embedded terminal appears as a resizable bottom drawer that spans the content region to the right of the sidebar, below both pane layouts and above the status bar, following Dolphin's panel placement.
Falsifier: opening the terminal replaces content, covers the status bar, occupies only one pane, or creates a separate window.
Mechanism: shell layout and resize tests with single-pane and dual-pane windows.
Status: Draft

## Menus, dialogs, and settings

[UIV-020] Context menus use compact rows, stable command grouping, submenu arrows, shortcut labels, destructive styling, check or radio state, and an accessible disabled reason when the reason matters.
Falsifier: a long menu loses grouping, state, shortcut, or destructive distinction.
Mechanism: populated menu gallery for every target kind and theme.
Status: Draft

[UIV-021] Properties uses a resizable window with a persistent item identity header, page navigation, selectable read-only values, and an Apply button only when editable fields are dirty and valid.
Falsifier: changing pages loses edits, a read-only path cannot be copied, or invalid metadata can be applied.
Mechanism: file, directory, multi-selection, mount, and remote Properties tests.
Status: Draft

[UIV-022] The Settings window uses a searchable page sidebar, page title, grouped controls, per-page reset, and restart-required labels. Search results show the owning page and navigate to and focus the selected setting.
Falsifier: a matching setting cannot be located from search or the UI hides the scope of a reset or restart requirement.
Mechanism: Settings state gallery and search-to-control interaction tests.
Status: Draft

[UIV-023] Elevated browser surfaces display an always-visible warning banner and privilege icon that cannot be themed to match ordinary chrome exactly.
Falsifier: an elevated and ordinary surface are visually indistinguishable.
Mechanism: paired screenshots in light, dark, and high-contrast themes.
Status: Draft

[UIV-025] Laid-out text places each glyph on a whole pixel, as Qt does on Plasma, so a word in the desktop's font takes the width it takes in Dolphin.
Falsifier: a glyph of laid-out text starts at a fractional pixel, or a line's width differs from the sum of its glyphs' whole-pixel advances.
Mechanism: uiv-025
Rationale: Shawn's report of 2026-09-27 (item linux-native-look): the font looked different from Dolphin's. The same words had the same height in both but were about 10% narrower in Musheen, because GPUI shaped text with cosmic-text's metrics hinting off, which leaves glyphs at fractional positions.
Status: Agreed 2026-09-28

[UIV-024] The shell remains operable at the minimum supported width of 720 logical pixels and at 200 percent scale; narrower windows may refuse further resize rather than clipping required navigation, status, or recovery controls.
Falsifier: a supported size clips or makes a required command unreachable.
Mechanism: width-by-scale layout sweep including translated long labels.
Status: Draft
