mod adaptive;
mod columns;
mod details;
mod grid;
mod group;
mod list;
mod selection;
mod sort;

pub use adaptive::AdaptiveLayout;
pub(crate) use columns::{COLUMN_CACHE_ROWS, COLUMN_RESIDENT_BUDGET, ColumnPaneItems, ColumnTrail};
pub use columns::{ColumnKey, ColumnLayout, ColumnLayoutError, ColumnsPresentation};
pub use details::DetailsPresentation;
pub use grid::GridPresentation;
pub use group::GroupKey;
pub use list::ListPresentation;
pub use selection::SelectionMode;
pub use sort::{SortDirection, SortKey, SortSpec};

use musheen_core::{ItemId, ItemKind, StoreItem, StorePath, WatchEvent};
use musheen_desktop::{
    FolderIdentity, FolderPreferenceCatalog, FolderSortDirection, FolderSortKey, FolderView,
};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::{Range, RangeInclusive};
use std::sync::Arc;

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

    pub(crate) fn set_defaults(&mut self, preferences: ViewPreferences) {
        self.defaults = preferences;
    }

    /// Migrates the durable view/sort portion of a catalog preference into the
    /// existing session model. Session-only presentation fields remain intact.
    pub fn apply_catalog(
        &mut self,
        identity: &FolderIdentity,
        path: StorePath,
        catalog: &FolderPreferenceCatalog,
    ) {
        let Some(durable) = catalog.resolve_recorded(identity) else {
            return;
        };
        let mut preferences = self.for_path(&path).clone();
        preferences.layout = match durable.view() {
            FolderView::Details => Layout::Details,
            FolderView::List => Layout::List,
            FolderView::Cards => Layout::Cards,
            FolderView::Grid => Layout::Grid,
            FolderView::Columns => Layout::Columns,
            FolderView::Adaptive => Layout::Adaptive,
        };
        preferences.icon_size = durable.icon_size();
        preferences.sort.key = match durable.sort_key() {
            FolderSortKey::Name => SortKey::Name,
            FolderSortKey::Size => SortKey::Size,
            FolderSortKey::Kind => SortKey::Kind,
            FolderSortKey::Modified => SortKey::Modified,
        };
        preferences.sort.direction = match durable.sort_direction() {
            FolderSortDirection::Ascending => SortDirection::Ascending,
            FolderSortDirection::Descending => SortDirection::Descending,
        };
        self.set(path, preferences);
    }

    pub fn persist_catalog(
        &self,
        identity: FolderIdentity,
        path: StorePath,
        parent: Option<FolderIdentity>,
        catalog: &mut FolderPreferenceCatalog,
    ) {
        let preferences = self.for_path(&path);
        let view = match preferences.layout {
            Layout::Details => FolderView::Details,
            Layout::List => FolderView::List,
            Layout::Cards => FolderView::Cards,
            Layout::Grid => FolderView::Grid,
            Layout::Columns => FolderView::Columns,
            Layout::Adaptive => FolderView::Adaptive,
        };
        let sort_key = match preferences.sort.key {
            SortKey::Name => FolderSortKey::Name,
            SortKey::Size => FolderSortKey::Size,
            SortKey::Kind => FolderSortKey::Kind,
            SortKey::Modified => FolderSortKey::Modified,
        };
        let sort_direction = match preferences.sort.direction {
            SortDirection::Ascending => FolderSortDirection::Ascending,
            SortDirection::Descending => FolderSortDirection::Descending,
        };
        catalog.set(
            identity,
            path,
            parent,
            musheen_desktop::FolderPreference::new(view, sort_key, sort_direction)
                .with_icon_size(preferences.icon_size),
        );
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
    focused_item: Option<ItemId>,
    editing: Option<ItemId>,
    scroll_anchor: Option<ScrollAnchor>,
    complete: bool,
    visible_order: RefCell<Option<Arc<Vec<usize>>>>,
}

impl DirectoryViewModel {
    #[must_use]
    pub fn new(retention_limit: usize) -> Self {
        Self {
            retention_limit: retention_limit.max(1),
            items: Vec::new(),
            preferences: ViewPreferences::default(),
            selection: selection::SelectionModel::default(),
            focused_item: None,
            editing: None,
            scroll_anchor: None,
            complete: false,
            visible_order: RefCell::new(None),
        }
    }

    pub fn extend(&mut self, items: impl IntoIterator<Item = StoreItem>) {
        self.invalidate_visible_order();
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
        self.invalidate_visible_order();
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

    pub(crate) fn take_items_for_index(&mut self) -> Vec<StoreItem> {
        self.invalidate_visible_order();
        std::mem::take(&mut self.items)
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
        self.invalidate_visible_order();
        &mut self.preferences
    }

    #[must_use]
    pub fn visible_items(&self) -> Vec<&StoreItem> {
        self.ensure_visible_order();
        self.visible_order
            .borrow()
            .as_ref()
            .expect("visible order was populated")
            .iter()
            .map(|index| &self.items[*index])
            .collect()
    }

    /// A shared snapshot of sorted positions in `items`. Reusing this order
    /// avoids copying every item identity on each immediate-mode render.
    #[must_use]
    pub fn visible_item_indices(&self) -> Arc<Vec<usize>> {
        self.ensure_visible_order();
        Arc::clone(
            self.visible_order
                .borrow()
                .as_ref()
                .expect("visible order was populated"),
        )
    }

    /// Move the ordered visible items into a column pane before this view is
    /// reset for child navigation. Large directories must not clone every path
    /// and metadata record on the UI thread.
    pub(crate) fn take_visible_items(&mut self) -> Vec<StoreItem> {
        self.ensure_visible_order();
        let order = self.visible_order.get_mut().take().unwrap_or_default();
        let mut items = std::mem::take(&mut self.items)
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>();
        let visible = order
            .iter()
            .copied()
            .filter_map(|index| items.get_mut(index).and_then(Option::take))
            .collect();
        self.reset_items();
        visible
    }

    fn ensure_visible_order(&self) {
        if self.visible_order.borrow().is_some() {
            return;
        }
        let mut order = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                self.preferences.show_hidden || !item.display_name().as_str().starts_with('.')
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        order.sort_by(|left, right| self.compare_items(&self.items[*left], &self.items[*right]));
        *self.visible_order.borrow_mut() = Some(Arc::new(order));
    }

    /// Returns only the visible IDs addressed by `indices`. The cached order
    /// avoids rebuilding or cloning the full sorted view during pointer drags.
    #[must_use]
    pub fn visible_item_ids_at(&self, indices: &[usize]) -> Vec<ItemId> {
        self.ensure_visible_order();
        let order = self.visible_order.borrow();
        let order = order.as_ref().expect("visible order was populated");
        indices
            .iter()
            .filter_map(|index| order.get(*index))
            .map(|item_index| self.items[*item_index].id().clone())
            .collect()
    }

    pub fn toggle_details_sort(&mut self, column: ColumnKey) {
        self.invalidate_visible_order();
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

    pub fn set_selected_ids(&mut self, ids: Vec<ItemId>) {
        self.selection.apply(ids, SelectionMode::Replace);
        self.trim_unpinned();
    }

    pub fn rubber_band_select_indices(&mut self, indices: &[usize], mode: SelectionMode) {
        let ids = self.visible_item_ids_at(indices);
        self.rubber_band_select_ids(&ids, mode);
    }

    pub fn rubber_band_select_ids(&mut self, ids: &[ItemId], mode: SelectionMode) {
        let ids = ids
            .iter()
            .filter(|id| self.item(id).is_some())
            .cloned()
            .collect();
        self.selection.apply(ids, mode);
        self.trim_unpinned();
    }

    pub fn select_item(&mut self, id: ItemId, mode: SelectionMode) {
        self.selection.apply(vec![id], mode);
        self.trim_unpinned();
    }

    pub fn select_to_item(&mut self, id: &ItemId, mode: SelectionMode) {
        let order = self
            .visible_items()
            .into_iter()
            .map(|item| item.id().clone())
            .collect::<Vec<_>>();
        self.select_to_item_in_order(id, &order, mode);
    }

    pub fn select_to_item_in_order(&mut self, id: &ItemId, order: &[ItemId], mode: SelectionMode) {
        let Some(end) = order.iter().position(|candidate| candidate == id) else {
            return;
        };
        let original_anchor = self.selection.anchor().cloned();
        let start = original_anchor
            .as_ref()
            .and_then(|anchor| order.iter().position(|candidate| candidate == anchor))
            .unwrap_or(end);
        let ids = order[start.min(end)..=start.max(end)]
            .iter()
            .filter(|candidate| self.item(candidate).is_some())
            .cloned()
            .collect();
        self.selection.apply(ids, mode);
        if let Some(anchor) = original_anchor {
            self.selection.restore_anchor(anchor);
        }
        self.trim_unpinned();
    }

    #[must_use]
    pub fn focused_item_id(&self) -> Option<&ItemId> {
        self.focused_item.as_ref()
    }

    pub fn focus_item(&mut self, id: Option<ItemId>) {
        self.focused_item = id.filter(|id| self.item(id).is_some());
    }

    pub(crate) fn focus_indexed_item(&mut self, id: ItemId) {
        self.focused_item = Some(id);
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

    pub(crate) fn selection_anchor(&self) -> Option<&ItemId> {
        self.selection.anchor()
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
    pub fn has_retention_capacity(&self) -> bool {
        self.unpinned_model_count() < self.retention_limit
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
        Self::compare_with_preferences(&self.preferences, left, right)
    }

    pub(crate) fn compare_with_preferences(
        preferences: &ViewPreferences,
        left: &StoreItem,
        right: &StoreItem,
    ) -> std::cmp::Ordering {
        if preferences.directories_first {
            let left_directory = left.kind() == ItemKind::Directory;
            let right_directory = right.kind() == ItemKind::Directory;
            let directory_order = right_directory.cmp(&left_directory);
            if !directory_order.is_eq() {
                return directory_order;
            }
        }
        group::compare(preferences.group, left, right)
            .then_with(|| sort::compare(preferences.sort, left, right))
    }

    fn remove(&mut self, id: &ItemId) {
        self.invalidate_visible_order();
        let removed = self.items.iter().position(|item| item.id() == id);
        self.items.retain(|item| item.id() != id);
        self.selection.remove(id);
        if self.editing.as_ref() == Some(id) {
            self.editing = None;
        }
        if self.focused_item.as_ref() == Some(id) {
            self.focused_item = self.items.first().map(|item| item.id().clone());
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
        let mut excess = self.items.len().saturating_sub(self.retention_limit);
        if excess == 0 {
            return;
        }
        let pinned = self.pinned_ids();
        self.invalidate_visible_order();
        self.items.retain(|item| {
            let evict = excess > 0 && !pinned.contains(item.id());
            excess -= usize::from(evict);
            !evict
        });
        if excess > 0 {
            self.items.drain(..excess);
        }
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

    fn invalidate_visible_order(&mut self) {
        *self.visible_order.get_mut() = None;
    }
}
