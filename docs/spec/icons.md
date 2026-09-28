Prefix: ICON

# Icon system

Musheen uses one drawn language for its own interface. The primary family is
Lucide 1.43 bundled with GPUI Kit 0.6.2. Native Linux content icons remain a
separate identity layer for files, applications, and mounted media.

## Sources and roles

[ICON-001] Every command, toolbar, menu, sidebar-control, status, and settings icon uses the bundled Lucide family through a stable app-owned semantic icon registry.
Falsifier: a Musheen command renders a glyph from Evil Icons, TheSVG, SVGL, an emoji font, or another symbolic family.
Mechanism: registry provenance test plus rendered icon inventory review.
Status: Draft

[ICON-002] File, directory, MIME, application, device, and mount identity uses the active freedesktop icon theme. A file's type comes from shared-mime-info by its name, without reading its content. Its icon is the theme's icon for that type, named as the icon naming specification names it (image/png as image-png), then the type's generic icon (such as image-x-generic), each looked up in the theme, the themes it inherits, and hicolor. A type no theme has an icon for shows the bundled file-type icon for the file's extension, from the vivid style of file-icon-vectors (MIT), then the Lucide file-type glyph of its family (text, image, audio, video, archive, document, spreadsheet, presentation, code, or executable), and only a file that matches none of them shows the plain file glyph. Missing folder, drive, network, or cloud icons fall back to their Lucide glyphs.
Falsifier: a file whose type, or whose type's generic icon, the active theme or a theme it inherits has an icon for shows another icon; a file whose type no theme has an icon for shows something other than the bundled icon for its extension when one exists; a file in one of the listed families shows the plain file glyph; finding a file's icon reads its content; or a missing icon produces a blank item or a second command-icon family.
Mechanism: icon-002
Rationale: Shawn's rulings of 2026-09-28 (item linux-native-look): file-type icons at least, monochrome is fine, and the vivid file-icon-vectors set he chose for types the theme lacks. The list asked the theme for image/png instead of image-png, so every file fell back to one generic glyph.
Status: Agreed 2026-09-28

[ICON-003] The icon registry maps semantic roles to Lucide names once; views request a role and never load an SVG path or choose a near-duplicate directly.
Falsifier: two views map the same command to different glyphs or embed a raw command SVG outside the registry.
Mechanism: source-boundary check and role-to-command uniqueness test.
Status: Draft

[ICON-004] Lucide icons keep their 24-unit grid, round joins and caps, and two-unit stroke. Musheen may compose a small badge but does not redraw or mix stroke weights inside one icon.
Falsifier: an app-owned edit changes the family geometry or a composed icon contains visibly different stroke rules.
Mechanism: SVG attribute audit and icon gallery review at every supported size.
Status: Draft

[ICON-005] Symbolic icons inherit named theme foreground, muted, accent, warning, and destructive tokens; source SVGs contain no product colors. High-contrast mode may increase stroke and boundary contrast through one renderer policy.
Falsifier: a Lucide asset hardcodes a UI color or becomes illegible in a supported native theme.
Mechanism: SVG color audit and light, dark, high-contrast theme screenshots.
Status: Draft

[ICON-006] The renderer provides optical sizes of 16, 20, and 24 logical pixels without distortion. The surrounding control, not the glyph, supplies the required pointer target and focus indicator.
Falsifier: a glyph is stretched, clipped, or used as an unlabeled pointer target smaller than the control standard.
Mechanism: size gallery, pixel-boundary, focus, and target-size tests.
Status: Draft

[ICON-007] Directional icons such as back, forward, undo, redo, send, and pane movement mirror in right-to-left UI. Physical concepts such as file, terminal, playback, sort direction, and filesystem paths do not mirror.
Falsifier: RTL changes a physical or data direction, or leaves a navigation direction pointing against the mirrored layout.
Mechanism: semantic mirror-list test plus paired LTR and RTL screenshots.
Status: Draft

[ICON-008] Checked, selected, busy, disabled, warning, and destructive state uses theme color, control background, badge, or motion without swapping to a different icon family or relying on color alone.
Falsifier: state is conveyed only by color or by an unrelated filled glyph.
Mechanism: state gallery and grayscale plus accessibility review.
Status: Draft

## Core mapping

| Role | Lucide name |
|---|---|
| Back / Forward / Parent | `arrow-left` / `arrow-right` / `arrow-up` |
| Refresh / Search / Settings | `refresh-cw` / `search` / `settings` |
| List / Grid / Columns / Dual pane | `list` / `grid-2x2` / `columns-3` / `columns-2` |
| New directory / New file | `folder-plus` / `file-plus` |
| Cut / Copy / Paste / Rename | `scissors` / `copy` / `clipboard-paste` / `file-pen-line` |
| Symbolic link / Hard link | `file-symlink` / `link-2` |
| Compress / Extract | `file-archive` / `archive-restore` |
| Trash / Permanent delete | `trash` / `shredder` |
| Properties / Permissions / Administrator | `info` / `shield-keyhole` / `shield` |
| Hide / Unhide / Terminal | `eye-off` / `eye` / `terminal` |
| Pin / Tag / Local drive / Network / Remote | `pin` / `tag` / `hard-drive` / `network` / `cloud` |

[ICON-009] TheSVG and SVGL assets may appear only as provider or service brand marks after per-asset license and trademark review. They never replace commands, file types, or generic locations, and monochrome contexts use text or a generic Lucide provider glyph instead of recoloring a restricted mark.
Falsifier: a catalog logo is used as generic UI, lacks recorded provenance, or is modified contrary to its terms.
Mechanism: brand-asset manifest audit with source, license, trademark rule, usage role, and file digest.
Status: Draft

[ICON-010] The icon coverage test requires a registry entry, accessible label, and available asset for every registered visible command. Unknown extension commands receive a generic `puzzle` glyph until they supply a reviewed icon.
Falsifier: a command renders blank, guesses an asset from its label, or lacks an accessible name.
Mechanism: command-to-icon completeness test and extension fallback fixture.
Status: Draft

[ICON-011] The application identity uses the repository-owned `assets/icons/musheen.svg` folder-and-machine mark. It is the only branded product mark, remains stable across native themes, and ships as scalable SVG plus packaging-generated raster sizes. Command icons remain symbolic.
Falsifier: packaging substitutes a generic gear, themes recolor the brand mark, or a required raster size is hand-edited rather than generated.
Mechanism: asset digest, SVG render, packaging generation, and 16 through 512 pixel legibility tests.
Status: Draft
