Status: Draft
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

[UIV-001]
Status: Draft
Each window arranges the tab strip above navigation, places the sidebar
beside the active content surface, and places the status bar below content.
Falsifier: a normal desktop window presents those primary regions in a different hierarchy.
Mechanism: shell hierarchy assertions plus repo-owned screenshots at standard widths.

[UIV-002]
Status: Draft
The navigation row groups back, forward, parent, refresh, breadcrumbs or
omnibar content, search, and view controls by task.
Falsifier: a listed navigation control appears in an unrelated content or mutation group.
Mechanism: component-tree assertion and shell screenshot review.

[UIV-003]
Status: Draft
The command bar presents frequent file actions before view, sort, group,
and pane controls.
Falsifier: view configuration interrupts the primary mutation-action group.
Mechanism: command-bar order test for normal and compact widths.

[UIV-004]
Status: Draft
The sidebar renders labeled, collapsible sections for places, pinned
locations, mounts, remote locations, network, and tags.
Falsifier: items from distinct sections form an unlabeled continuous list.
Mechanism: sidebar component test with every section populated.

[UIV-005]
Status: Draft
The content uses the primary surface token; sidebar, toolbar, dialogs, and
status regions use their named chrome or overlay tokens and theme-supplied separators.
Falsifier: a shell region hardcodes emphasis outside its assigned token role.
Mechanism: component token-role assertions plus theme screenshots.

## Components and states

[UIV-006]
Status: Draft
Interactive components define default, hover, pressed, focused, selected,
disabled, drag-target, and error states when those states apply.
Falsifier: an applicable state has no visual distinction from default.
Mechanism: component-state gallery screenshots in light, dark, and high-contrast themes.

[UIV-007]
Status: Draft
Selection uses the theme accent with a visible boundary that remains
distinct from hover and keyboard focus.
Falsifier: a selected item cannot be distinguished from hover or focus alone.
Mechanism: contrast calculation plus item-state screenshot comparison.

[UIV-008]
Status: Draft
All command and chrome icons use the Lucide family defined in `icons.md`.
Native content icons and reviewed provider marks stay within the role
boundaries defined there. Unfamiliar and destructive symbols have accessible
labels or tooltips.
Falsifier: equivalent actions use conflicting icon styles, an exception
appears outside its allowed role, or an ambiguous icon has no text alternative.
Mechanism: icon-registry, role-boundary, and accessible-name tests.

[UIV-009]
Status: Draft
Rounded corners, border weight, spacing, type scale, and elevation come
from shared design tokens.
Falsifier: a shell component hardcodes one of those values outside the token layer.
Mechanism: source scan plus token-coverage test for shell components.

[UIV-010]
Status: Draft
Directory layouts share selection, focus, label, thumbnail, and metadata
components even when their spatial arrangement differs.
Falsifier: switching layout changes the visual meaning of a shared item state.
Mechanism: cross-layout state gallery and component identity assertions.

## Responsive layout

[UIV-011]
Status: Draft
At narrow supported widths from 720 through 959 logical pixels, the command
bar moves lower-priority actions into an overflow menu without hiding the
active operation or navigation state.
Falsifier: resizing makes an active or required command unreachable.
Mechanism: width-sweep layout test with every command enabled.

[UIV-012]
Status: Draft
The sidebar and info pane can collapse independently while the content
surface retains a visible way to restore either pane.
Falsifier: a collapsed pane cannot be restored without reopening the window.
Mechanism: responsive interaction test across minimum, medium, and wide widths.

[UIV-013]
Status: Draft
Dual-pane mode is available at 960 logical pixels or wider and maintains a
visible divider and distinct focus treatment for the active pane. Below 960,
the second pane remains in the model but is hidden behind a pane switcher.
Falsifier: the user cannot identify or reach the focused pane, or resize the
pane boundary where both panes are visible.
Mechanism: dual-pane screenshots, pane-switcher tests, and divider interaction tests.

## Theme, motion, and content states

[UIV-014]
Status: Draft
The theme layer derives every available system appearance input from the
native-theme bridge and uses named fallback tokens for values it does not
provide.
Falsifier: a component copies a Files color or Windows material instead of using a theme token.
Mechanism: theme source audit plus token-resolution tests with complete and partial native themes.

[UIV-015]
Status: Draft
Text, icons, focus indicators, and essential boundaries meet WCAG AA
contrast in light, dark, and high-contrast themes.
Falsifier: any listed essential element falls below its applicable AA ratio.
Mechanism: automated contrast audit over the component-state gallery.

[UIV-016]
Status: Draft
Motion communicates spatial change or operation state and becomes instant
or minimal when the system requests reduced motion.
Falsifier: decorative or large movement continues unchanged under reduced motion.
Mechanism: animation-duration audit and reduced-motion interaction tests.

[UIV-017]
Status: Draft
Directory and pane surfaces define loading, empty, partial, offline, and
error presentations that preserve available navigation and recovery actions.
Falsifier: a non-success state replaces the surface with an unactionable blank region.
Mechanism: state screenshots and action-presence tests for every listed presentation.

[UIV-018]
Status: Draft
The visual regression suite captures the shell and core component states
at standard and narrow widths in light, dark, and high-contrast themes.
Falsifier: a shell region or applicable component state has no maintained baseline.
Mechanism: baseline inventory check plus deterministic screenshot test run.

## Terminal drawer

[UIV-019]
Status: Draft
The embedded terminal appears as a resizable bottom drawer that spans the
content region to the right of the sidebar, below both pane layouts and above
the status bar, following Dolphin's panel placement.
Falsifier: opening the terminal replaces content, covers the status bar,
occupies only one pane, or creates a separate window.
Mechanism: shell layout and resize tests with single-pane and dual-pane windows.

## Menus, dialogs, and settings

[UIV-020]
Status: Draft
Context menus use compact rows, stable command grouping, submenu arrows,
shortcut labels, destructive styling, check or radio state, and an
accessible disabled reason when the reason matters.
Falsifier: a long menu loses grouping, state, shortcut, or destructive distinction.
Mechanism: populated menu gallery for every target kind and theme.

[UIV-021]
Status: Draft
Properties uses a resizable window with a persistent item identity header,
page navigation, selectable read-only values, and an Apply button only when
editable fields are dirty and valid.
Falsifier: changing pages loses edits, a read-only path cannot be copied,
or invalid metadata can be applied.
Mechanism: file, directory, multi-selection, mount, and remote Properties tests.

[UIV-022]
Status: Draft
The Settings window uses a searchable page sidebar, page title, grouped
controls, per-page reset, and restart-required labels. Search results show
the owning page and navigate to and focus the selected setting.
Falsifier: a matching setting cannot be located from search or the UI hides
the scope of a reset or restart requirement.
Mechanism: Settings state gallery and search-to-control interaction tests.

[UIV-023]
Status: Draft
Elevated browser surfaces display an always-visible warning banner and
privilege icon that cannot be themed to match ordinary chrome exactly.
Falsifier: an elevated and ordinary surface are visually indistinguishable.
Mechanism: paired screenshots in light, dark, and high-contrast themes.

[UIV-024]
Status: Draft
The shell remains operable at the minimum supported width of 720 logical
pixels and at 200 percent scale; narrower windows may refuse further resize
rather than clipping required navigation, status, or recovery controls.
Falsifier: a supported size clips or makes a required command unreachable.
Mechanism: width-by-scale layout sweep including translated long labels.
