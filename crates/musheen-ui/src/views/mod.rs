mod adaptive;
mod columns;
mod details;
mod grid;
mod group;
mod list;
mod selection;
mod sort;

pub use adaptive::AdaptiveLayout;
pub use columns::{ColumnKey, ColumnLayout, ColumnLayoutError, ColumnsPresentation};
pub use details::DetailsPresentation;
pub use grid::GridPresentation;
pub use group::GroupKey;
pub use list::ListPresentation;
pub use selection::SelectionMode;
pub use sort::{SortDirection, SortKey, SortSpec};

use musheen_core::{ItemId, ItemKind, StoreItem, StorePath, WatchEvent};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ops::{Range, RangeInclusive};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum Layout {
    Details,
    List,
    Cards,
    #[default]
    Grid,
    Columns,
    Adaptive,
}

impl Layout {
    pub const ALL: [Self; 6] = [
        Self::Details,
        Self::List,
        Self::Cards,
        Self::Grid,
        Self::Columns,
        Self::Adaptive,
    ];
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ViewPreferences {
    pub layout: Layout,
    pub icon_size: u16,
    pub sort: SortSpec,
    pub group: GroupKey,
    pub directories_first: bool,
    pub show_hidden: bool,
    pub columns: ColumnLayout,
}

impl Default for ViewPreferences {
    fn default() -> Self {
        Self {
            layout: Layout::Grid,
            icon_size: 48,
            sort: SortSpec::default(),
            group: GroupKey::None,
            directories_first: true,
            show_hidden: false,
            columns: ColumnLayout::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ViewPreferenceStore {
    defaults: ViewPreferences,
    directories: Vec<(StorePath, ViewPreferences)>,
}

impl ViewPreferenceStore {
    #[must_use]
    pub fn new(defaults: ViewPreferences) -> Self {
        Self {
            defaults,
            directories: Vec::new(),
        }
    }

    pub fn set(&mut self, path: StorePath, preferences: ViewPreferences) {
        if let Some((_, stored)) = self
            .directories
            .iter_mut()
            .find(|(stored, _)| stored == &path)
        {
            *stored = preferences;
        } else {
            self.directories.push((path, preferences));
        }
    }

    #[must_use]
    pub fn for_path(&self, path: &StorePath) -> &ViewPreferences {
        self.directories
            .iter()
            .find(|(stored, _)| stored == path)
            .map(|(_, preferences)| preferences)
            .unwrap_or(&self.defaults)
    }

    #[must_use]
    pub const fn defaults(&self) -> &ViewPreferences {
        &self.defaults
    }
}

impl Default for ViewPreferenceStore {
    fn default() -> Self {
        Self::new(ViewPreferences::default())
    }
}

#[derive(Clone, Debug)]
pub struct ScrollAnchor {
    item: ItemId,
    offset: f32,
}

impl ScrollAnchor {
    #[must_use]
    pub const fn item(&self) -> &ItemId {
        &self.item
    }

    #[must_use]
    pub const fn offset(&self) -> f32 {
        self.offset
    }
}

#[derive(Clone, Debug)]
pub struct DirectoryViewModel {
    retention_limit: usize,
    items: Vec<StoreItem>,
    preferences: ViewPreferences,
    selection: selection::SelectionModel,
    editing: Option<ItemId>,
    scroll_anchor: Option<ScrollAnchor>,
    complete: bool,
}

impl DirectoryViewModel {
    #[must_use]
    pub fn new(retention_limit: usize) -> Self {
        Self {
            retention_limit: retention_limit.max(1),
            items: Vec::new(),
            preferences: ViewPreferences::default(),
            selection: selection::SelectionModel::default(),
            editing: None,
            scroll_anchor: None,
            complete: false,
        }
    }

    pub fn extend(&mut self, items: impl IntoIterator<Item = StoreItem>) {
        let mut positions = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.id().clone(), index))
            .collect::<HashMap<_, _>>();
        for item in items {
            if let Some(index) = positions.get(item.id()).copied() {
                self.items[index] = item;
            } else {
                positions.insert(item.id().clone(), self.items.len());
                self.items.push(item);
            }
        }
        self.trim_unpinned();
    }

    pub fn reset_items(&mut self) {
        self.items.clear();
        self.selection.clear();
        self.editing = None;
        self.scroll_anchor = None;
        self.complete = false;
    }

    #[must_use]
    pub fn items(&self) -> &[StoreItem] {
        &self.items
    }

    #[must_use]
    pub fn visible_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| {
                self.preferences.show_hidden || !item.display_name().as_str().starts_with('.')
            })
            .count()
    }

    #[must_use]
    pub const fn preferences(&self) -> &ViewPreferences {
        &self.preferences
    }

    pub fn preferences_mut(&mut self) -> &mut ViewPreferences {
        &mut self.preferences
    }

    #[must_use]
    pub fn visible_items(&self) -> Vec<&StoreItem> {
        let mut items = self
            .items
            .iter()
            .filter(|item| {
                self.preferences.show_hidden || !item.display_name().as_str().starts_with('.')
            })
            .collect::<Vec<_>>();
        items.sort_by(|left, right| self.compare_items(left, right));
        items
    }

    pub fn toggle_details_sort(&mut self, column: ColumnKey) {
        let key = match column {
            ColumnKey::Name => SortKey::Name,
            ColumnKey::Size => SortKey::Size,
            ColumnKey::Kind => SortKey::Kind,
            ColumnKey::Modified => SortKey::Modified,
        };
        if self.preferences.sort.key == key {
            self.preferences.sort.direction = match self.preferences.sort.direction {
                SortDirection::Ascending => SortDirection::Descending,
                SortDirection::Descending => SortDirection::Ascending,
            };
        } else {
            self.preferences.sort = SortSpec {
                key,
                direction: SortDirection::Ascending,
            };
        }
    }

    pub fn select_visible_range(&mut self, range: RangeInclusive<usize>, mode: SelectionMode) {
        let ids = self
            .visible_items()
            .get(range)
            .unwrap_or_default()
            .iter()
            .map(|item| item.id().clone())
            .collect();
        self.selection.apply(ids, mode);
        self.trim_unpinned();
    }

    pub fn rubber_band_select(&mut self, range: RangeInclusive<usize>, mode: SelectionMode) {
        self.select_visible_range(range, mode);
    }

    pub fn select_item(&mut self, id: ItemId, mode: SelectionMode) {
        if self.item(&id).is_some() {
            self.selection.apply(vec![id], mode);
            self.trim_unpinned();
        }
    }

    pub fn select_all_visible(&mut self) {
        let ids = self
            .visible_items()
            .into_iter()
            .map(|item| item.id().clone())
            .collect();
        self.selection.apply(ids, SelectionMode::Replace);
    }

    pub fn clear_selection(&mut self) {
        self.selection.clear();
    }

    pub fn set_editing(&mut self, editing: Option<ItemId>) {
        self.editing = editing;
        self.trim_unpinned();
    }

    #[must_use]
    pub fn editing(&self) -> Option<&ItemId> {
        self.editing.as_ref()
    }

    pub fn set_scroll_anchor(&mut self, item: Option<ItemId>, offset: f32) {
        self.scroll_anchor = item.map(|item| ScrollAnchor { item, offset });
        self.trim_unpinned();
    }

    #[must_use]
    pub fn scroll_anchor(&self) -> Option<&ScrollAnchor> {
        self.scroll_anchor.as_ref()
    }

    #[must_use]
    pub fn selected_ids(&self) -> &[ItemId] {
        self.selection.ids()
    }

    #[must_use]
    pub fn item(&self, id: &ItemId) -> Option<&StoreItem> {
        self.items.iter().find(|item| item.id() == id)
    }

    #[must_use]
    pub fn unpinned_model_count(&self) -> usize {
        self.items
            .iter()
            .filter(|item| !self.is_pinned(item.id()))
            .count()
    }

    #[must_use]
    pub fn rendered_range(&self, first_visible: usize, viewport_items: usize) -> Range<usize> {
        let length = self.visible_items().len();
        let start = first_visible.min(length);
        start
            ..start
                .saturating_add(viewport_items.saturating_mul(3))
                .min(length)
    }

    pub fn apply_watch_event(&mut self, event: WatchEvent) {
        match event {
            WatchEvent::Created(item) | WatchEvent::Changed(item) => self.extend([item]),
            WatchEvent::Renamed { item, .. } => self.extend([item]),
            WatchEvent::Removed(id) => self.remove(&id),
            WatchEvent::Invalidated { .. } => self.complete = false,
        }
    }

    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    pub fn set_complete(&mut self, complete: bool) {
        self.complete = complete;
    }

    fn compare_items(&self, left: &StoreItem, right: &StoreItem) -> std::cmp::Ordering {
        if self.preferences.directories_first {
            let left_directory = left.kind() == ItemKind::Directory;
            let right_directory = right.kind() == ItemKind::Directory;
            let directory_order = right_directory.cmp(&left_directory);
            if !directory_order.is_eq() {
                return directory_order;
            }
        }
        group::compare(self.preferences.group, left, right)
            .then_with(|| sort::compare(self.preferences.sort, left, right))
    }

    fn remove(&mut self, id: &ItemId) {
        let removed = self.items.iter().position(|item| item.id() == id);
        self.items.retain(|item| item.id() != id);
        self.selection.remove(id);
        if self.editing.as_ref() == Some(id) {
            self.editing = None;
        }
        if self.scroll_anchor.as_ref().map(ScrollAnchor::item) == Some(id) {
            let replacement = removed.and_then(|index| {
                self.items
                    .get(index.min(self.items.len().saturating_sub(1)))
                    .map(|item| item.id().clone())
            });
            self.scroll_anchor = replacement.map(|item| ScrollAnchor { item, offset: 0.0 });
        }
    }

    fn trim_unpinned(&mut self) {
        let pinned = self.pinned_ids();
        let mut unpinned = self
            .items
            .iter()
            .filter(|item| !pinned.contains(item.id()))
            .count();
        if unpinned <= self.retention_limit {
            return;
        }
        self.items.retain(|item| {
            if pinned.contains(item.id()) || unpinned <= self.retention_limit {
                true
            } else {
                unpinned -= 1;
                false
            }
        });
        let present = self
            .items
            .iter()
            .map(|item| item.id().clone())
            .collect::<HashSet<_>>();
        self.selection.retain(|id| present.contains(id));
    }

    fn pinned_ids(&self) -> HashSet<ItemId> {
        let mut pinned = self.selection.ids().iter().cloned().collect::<HashSet<_>>();
        pinned.extend(self.editing.iter().cloned());
        pinned.extend(self.scroll_anchor.iter().map(|anchor| anchor.item.clone()));
        pinned
    }

    fn is_pinned(&self, id: &ItemId) -> bool {
        self.selection.contains(id)
            || self.editing.as_ref() == Some(id)
            || self.scroll_anchor.as_ref().map(ScrollAnchor::item) == Some(id)
    }
}
