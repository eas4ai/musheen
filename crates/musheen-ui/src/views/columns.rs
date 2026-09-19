use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum ColumnKey {
    Name,
    Size,
    Kind,
    Modified,
}

impl ColumnKey {
    pub const ALL: [Self; 4] = [Self::Name, Self::Size, Self::Kind, Self::Modified];
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ColumnSpec {
    key: ColumnKey,
    width: u16,
    visible: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ColumnLayout {
    columns: Vec<ColumnSpec>,
}

impl ColumnLayout {
    #[must_use]
    pub fn is_visible(&self, key: ColumnKey) -> bool {
        self.columns
            .iter()
            .find(|column| column.key == key)
            .is_some_and(|column| column.visible)
    }

    #[must_use]
    pub fn width(&self, key: ColumnKey) -> Option<u16> {
        self.columns
            .iter()
            .find(|column| column.key == key)
            .map(|column| column.width)
    }

    pub fn resize(&mut self, key: ColumnKey, width: f32) -> Result<(), ColumnLayoutError> {
        if !width.is_finite() || !(48.0..=1_024.0).contains(&width) {
            return Err(ColumnLayoutError::InvalidWidth);
        }
        self.find_mut(key)?.width = width.round() as u16;
        Ok(())
    }

    pub fn hide(&mut self, key: ColumnKey) -> Result<(), ColumnLayoutError> {
        if key == ColumnKey::Name {
            return Err(ColumnLayoutError::RequiredColumn);
        }
        self.find_mut(key)?.visible = false;
        Ok(())
    }

    pub fn show(&mut self, key: ColumnKey) -> Result<(), ColumnLayoutError> {
        self.find_mut(key)?.visible = true;
        Ok(())
    }

    pub fn move_before(
        &mut self,
        moved: ColumnKey,
        before: ColumnKey,
    ) -> Result<(), ColumnLayoutError> {
        let moved_index = self.index_of(moved)?;
        let column = self.columns.remove(moved_index);
        let before_index = self.index_of(before)?;
        self.columns.insert(before_index, column);
        Ok(())
    }

    pub fn move_left(&mut self, key: ColumnKey) -> Result<(), ColumnLayoutError> {
        let index = self.index_of(key)?;
        if index > 0 {
            self.columns.swap(index, index - 1);
        }
        Ok(())
    }

    pub fn move_right(&mut self, key: ColumnKey) -> Result<(), ColumnLayoutError> {
        let index = self.index_of(key)?;
        if index + 1 < self.columns.len() {
            self.columns.swap(index, index + 1);
        }
        Ok(())
    }

    #[must_use]
    pub fn visible_columns(&self) -> Vec<ColumnKey> {
        self.columns
            .iter()
            .filter(|column| column.visible)
            .map(|column| column.key)
            .collect()
    }

    #[must_use]
    pub fn visible_columns_with_widths(&self) -> Vec<(ColumnKey, u16)> {
        self.columns
            .iter()
            .filter(|column| column.visible)
            .map(|column| (column.key, column.width))
            .collect()
    }

    fn index_of(&self, key: ColumnKey) -> Result<usize, ColumnLayoutError> {
        self.columns
            .iter()
            .position(|column| column.key == key)
            .ok_or(ColumnLayoutError::UnknownColumn)
    }

    fn find_mut(&mut self, key: ColumnKey) -> Result<&mut ColumnSpec, ColumnLayoutError> {
        self.columns
            .iter_mut()
            .find(|column| column.key == key)
            .ok_or(ColumnLayoutError::UnknownColumn)
    }
}

impl Default for ColumnLayout {
    fn default() -> Self {
        Self {
            columns: vec![
                ColumnSpec {
                    key: ColumnKey::Name,
                    width: 240,
                    visible: true,
                },
                ColumnSpec {
                    key: ColumnKey::Size,
                    width: 96,
                    visible: true,
                },
                ColumnSpec {
                    key: ColumnKey::Kind,
                    width: 120,
                    visible: true,
                },
                ColumnSpec {
                    key: ColumnKey::Modified,
                    width: 160,
                    visible: true,
                },
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColumnLayoutError {
    UnknownColumn,
    RequiredColumn,
    InvalidWidth,
}

impl fmt::Display for ColumnLayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnknownColumn => "column does not exist",
            Self::RequiredColumn => "the name column must remain visible",
            Self::InvalidWidth => "column width must be between 48 and 1024 logical pixels",
        })
    }
}

impl std::error::Error for ColumnLayoutError {}

pub struct ColumnsPresentation;
