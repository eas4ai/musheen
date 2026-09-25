use std::collections::BTreeMap;

use musheen_desktop::TerminalCell;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalRenderLine {
    row: i32,
    text: String,
}

impl TerminalRenderLine {
    #[must_use]
    pub const fn row(&self) -> i32 {
        self.row
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Projects Alacritty's cell grid into immutable rows consumed by GPUI.
#[must_use]
pub fn project_terminal_cells(cells: &[TerminalCell]) -> Vec<TerminalRenderLine> {
    let mut rows = BTreeMap::<i32, Vec<(usize, char, u8)>>::new();
    for cell in cells {
        rows.entry(cell.row())
            .or_default()
            .push((cell.column(), cell.character(), cell.width()));
    }
    rows.into_iter()
        .map(|(row, mut cells)| {
            cells.sort_by_key(|(column, _, _)| *column);
            let mut text = String::new();
            let mut display_column = 0;
            for (column, character, width) in cells {
                while display_column < column {
                    text.push(' ');
                    display_column += 1;
                }
                text.push(character);
                display_column = column + usize::from(width);
            }
            TerminalRenderLine {
                row,
                text: text.trim_end().to_owned(),
            }
        })
        .collect()
}
