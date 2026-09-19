# Patch native-theme GPUI connector for GPUI Kit 0.6.2

Level: Judged
Decided by: agent
Rests on: DEP-001 and the GPUI Kit 0.6.2 architecture choice
Would be wrong if: A released native-theme-gpui 0.5.x already supports gpui-component 0.6.2 or removing the obsolete tiles mapping changes a still-supported GPUI surface

## Decision

Vendor native-theme-gpui 0.5.8 and remove only the two assignments to ThemeColor.tiles and ThemeConfigColors.tiles. GPUI Kit removed the tiles canvas and both fields in 0.6.2. Keep all other connector code unchanged, preserve the upstream license files, and remove the patch when a compatible 0.5.x release is available.

## Realized by

- 77a45a31f3695a55618087ecfb7b54a86ea19e59 build: add native theme dependencies
- 443ca23b6b411b80a6e73ffac1e05dc1ee64b1c1 build: trim vendored theme connector
