use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

use crate::directory::DirectoryIndexReader;
use musheen_core::{ItemKind, StoreItem, StorePath};

/// Retain only the levels that can fit beside the active directory. Older
/// levels remain reachable through Back and the breadcrumb trail.
const MAX_PARENT_LEVELS: usize = 3;

/// Where a parent column takes its rows from: the items the folder held in
/// memory, or the disk index of a folder too large to keep in memory. An
/// indexed pane keeps the index alive for as long as the column shows it.
#[derive(Clone, Debug)]
pub(crate) enum ColumnPaneItems {
    InMemory(Arc<[StoreItem]>),
    Indexed {
        reader: DirectoryIndexReader,
        visible_count: usize,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct ColumnPane {
    location: StorePath,
    items: ColumnPaneItems,
    active_child: StorePath,
    complete: bool,
}

impl ColumnPane {
    #[must_use]
    pub(crate) fn location(&self) -> &StorePath {
        &self.location
    }

    /// The number of rows the column shows.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        match &self.items {
            ColumnPaneItems::InMemory(items) => items.len(),
            ColumnPaneItems::Indexed { visible_count, .. } => *visible_count,
        }
    }

    /// The rows of a pane that holds its items in memory.
    #[must_use]
    pub(crate) fn resident_items(&self) -> Option<&Arc<[StoreItem]>> {
        match &self.items {
            ColumnPaneItems::InMemory(items) => Some(items),
            ColumnPaneItems::Indexed { .. } => None,
        }
    }

    /// The index reader of a pane that shows an indexed folder.
    #[must_use]
    pub(crate) fn index_reader(&self) -> Option<&DirectoryIndexReader> {
        match &self.items {
            ColumnPaneItems::InMemory(_) => None,
            ColumnPaneItems::Indexed { reader, .. } => Some(reader),
        }
    }

    #[must_use]
    pub(crate) fn active_child(&self) -> &StorePath {
        &self.active_child
    }

    #[must_use]
    pub(crate) const fn is_complete(&self) -> bool {
        self.complete
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ColumnTrail {
    parents: Vec<ColumnPane>,
}

impl ColumnTrail {
    #[must_use]
    pub(crate) fn parents(&self) -> &[ColumnPane] {
        &self.parents
    }

    pub(crate) fn clear(&mut self) {
        self.parents.clear();
    }

    /// Keep the current directory only when the destination is one of its
    /// folders. Back navigation reuses the older levels without duplicating it.
    /// `items` is `None` when the caller found that the destination is not a
    /// folder of the current directory.
    pub(crate) fn navigate(
        &mut self,
        current: &StorePath,
        next: &StorePath,
        items: Option<ColumnPaneItems>,
        complete: bool,
    ) -> bool {
        if current == next {
            return true;
        }
        if let Some(index) = self.parents.iter().position(|pane| pane.location() == next) {
            self.parents.truncate(index);
            return true;
        }
        let descends = match &items {
            Some(ColumnPaneItems::InMemory(items)) => items
                .iter()
                .any(|item| item.kind() == ItemKind::Directory && item.path() == next),
            // The caller checked the destination against the indexed folder.
            Some(ColumnPaneItems::Indexed { .. }) => true,
            None => false,
        };
        let Some(items) = items.filter(|_| descends) else {
            self.clear();
            return false;
        };
        self.parents.push(ColumnPane {
            location: current.clone(),
            items,
            active_child: next.clone(),
            complete,
        });
        if self.parents.len() > MAX_PARENT_LEVELS {
            self.parents.remove(0);
        }
        true
    }

    /// A click in an older column selects a sibling without treating the
    /// previously active child as the source of the next navigation.
    pub(crate) fn select_from_parent(&mut self, index: usize, next: &StorePath) -> bool {
        let Some(pane) = self.parents.get(index) else {
            return false;
        };
        // A click in an indexed column comes from a rendered row of that
        // folder; the resident check applies to in-memory panes only.
        if pane.resident_items().is_some_and(|items| {
            !items
                .iter()
                .any(|item| item.kind() == ItemKind::Directory && item.path() == next)
        }) {
            return false;
        }
        self.parents.truncate(index + 1);
        self.parents[index].active_child = next.clone();
        true
    }
}

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

#[cfg(test)]
mod tests {
    use super::{ColumnPaneItems, ColumnTrail};
    use crate::views::DirectoryViewModel;
    use musheen_core::{DisplayPath, ItemId, ItemKind, ProviderId, StoreItem, StorePath};

    fn resident(items: impl IntoIterator<Item = StoreItem>) -> Option<ColumnPaneItems> {
        Some(ColumnPaneItems::InMemory(
            items.into_iter().collect::<Vec<_>>().into(),
        ))
    }

    fn directory(index: u64, path: &str) -> StoreItem {
        let provider = ProviderId::new("local").unwrap();
        let name = path.rsplit('/').next().unwrap();
        StoreItem::new(
            ItemId::new(provider, index.to_be_bytes()).unwrap(),
            StorePath::from_unix_path(path),
            DisplayPath::new(name),
            ItemKind::Directory,
            None,
        )
    }

    #[test]
    fn descending_and_backtracking_preserves_only_the_ancestor_chain() {
        let root = StorePath::from_unix_path("/root");
        let first = StorePath::from_unix_path("/root/first");
        let leaf = StorePath::from_unix_path("/root/first/leaf");
        let mut trail = ColumnTrail::default();

        trail.navigate(&root, &first, resident([directory(1, "/root/first")]), true);
        trail.navigate(
            &first,
            &leaf,
            resident([directory(2, "/root/first/leaf")]),
            true,
        );
        assert_eq!(trail.parents().len(), 2);
        assert_eq!(trail.parents()[0].active_child(), &first);
        assert_eq!(trail.parents()[1].active_child(), &leaf);

        trail.navigate(&leaf, &root, resident([]), true);
        assert!(trail.parents().is_empty());
    }

    #[test]
    fn sibling_selection_trims_younger_columns_and_rejects_unknown_targets() {
        let root = StorePath::from_unix_path("/root");
        let first = StorePath::from_unix_path("/root/first");
        let second = StorePath::from_unix_path("/root/second");
        let leaf = StorePath::from_unix_path("/root/first/leaf");
        let mut trail = ColumnTrail::default();
        trail.navigate(
            &root,
            &first,
            resident([directory(1, "/root/first"), directory(2, "/root/second")]),
            false,
        );
        trail.navigate(
            &first,
            &leaf,
            resident([directory(3, "/root/first/leaf")]),
            true,
        );

        assert!(!trail.select_from_parent(0, &StorePath::from_unix_path("/unknown")));
        assert!(trail.select_from_parent(0, &second));
        assert_eq!(trail.parents().len(), 1);
        assert_eq!(trail.parents()[0].active_child(), &second);
        assert!(!trail.parents()[0].is_complete());
    }

    #[test]
    fn unrelated_navigation_clears_cached_levels_and_depth_is_bounded() {
        let mut trail = ColumnTrail::default();
        let mut current_path = "/root".to_owned();
        for index in 0..8 {
            let next_path = format!("{current_path}/{index}");
            let next = StorePath::from_unix_path(&next_path);
            trail.navigate(
                &StorePath::from_unix_path(&current_path),
                &next,
                resident([directory(index + 1, &next_path)]),
                true,
            );
            current_path = next_path;
        }
        assert_eq!(trail.parents().len(), 3);
        trail.navigate(
            &StorePath::from_unix_path(&current_path),
            &StorePath::from_unix_path("/elsewhere"),
            resident([]),
            true,
        );
        assert!(trail.parents().is_empty());
    }

    #[test]
    fn moving_visible_items_preserves_sort_order_without_retaining_hidden_rows() {
        let mut view = DirectoryViewModel::new(10);
        let hidden = StoreItem::new(
            ItemId::new(ProviderId::new("local").unwrap(), 9_u64.to_be_bytes()).unwrap(),
            StorePath::from_unix_path("/root/.hidden"),
            DisplayPath::new(".hidden"),
            ItemKind::RegularFile,
            None,
        );
        view.extend([
            directory(2, "/root/zeta"),
            hidden,
            directory(1, "/root/alpha"),
        ]);
        let names = view
            .take_visible_items()
            .into_iter()
            .map(|item| item.display_name().as_str().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["alpha", "zeta"]);
        assert!(view.items().is_empty());
    }
}
