use crate::search::DirectoryFilter;
use crate::views::{DirectoryViewModel, ViewPreferences};
use musheen_core::{
    CancellationToken, CommandTargetRef, ItemId, Page, PageRequest, ResourceLimits, Store,
    StoreError, StoreItem, StorePath, WatchEvent,
};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

mod index;
mod termination;
pub(crate) use index::IndexedSelection;
use index::{DiskDirectoryIndex, ResolvedIndexedSelection};
pub(crate) use index::{directory_index_root, missing_index_root_error};
use index::sweep_stale_indexes;
use termination::install_index_cleanup_on_termination;

const MAX_RESIDENT_ITEMS: usize = 4_096;

/// Makes `root` ready to hold this process's indexes: the termination
/// watcher is installed, and the indexes an earlier process left behind
/// are removed. Each root is prepared once per process; the removed
/// directories are returned. A window's model construction calls it, so no
/// window can index a folder under a root that was not prepared.
pub(crate) fn prepare_index_root(root: &std::path::Path) -> std::io::Result<Vec<PathBuf>> {
    static PREPARED: std::sync::OnceLock<Mutex<std::collections::BTreeSet<PathBuf>>> =
        std::sync::OnceLock::new();
    let watcher = install_index_cleanup_on_termination();
    let first_time = PREPARED
        .get_or_init(|| Mutex::new(std::collections::BTreeSet::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(root.to_path_buf());
    let swept = if first_time {
        sweep_stale_indexes(root)
    } else {
        Ok(Vec::new())
    };
    watcher?;
    swept
}

type SharedIndex = Arc<Mutex<DiskDirectoryIndex>>;

#[derive(Clone)]
pub(crate) struct DirectoryIndexReader {
    index: SharedIndex,
}

impl std::fmt::Debug for DirectoryIndexReader {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DirectoryIndexReader")
    }
}

pub(crate) struct IndexedFocusMove {
    pub(crate) id: ItemId,
    pub(crate) position: usize,
    pub(crate) selection: Option<IndexedSelection>,
}

impl DirectoryIndexReader {
    /// An index that holds `items` in the order `preferences` gives, for a
    /// column that must not keep them in memory. It is written on the
    /// caller's thread; the items are at most one folder's resident bound.
    pub(crate) fn from_items(
        root: &std::path::Path,
        items: &[StoreItem],
        preferences: &ViewPreferences,
    ) -> std::io::Result<(Self, usize)> {
        let mut index = DiskDirectoryIndex::new_in(root)?;
        for (arrival, item) in (0u64..).zip(items) {
            index.append(item, arrival)?;
        }
        index.rebuild_order(preferences, None)?;
        let visible_count = index.visible_count().unwrap_or(0);
        Ok((
            Self {
                index: Arc::new(Mutex::new(index)),
            },
            visible_count,
        ))
    }

    #[cfg(test)]
    pub(crate) fn read_range(&self, range: Range<usize>) -> std::io::Result<Vec<StoreItem>> {
        self.index
            .lock()
            .map_err(|_| std::io::Error::other("directory index worker stopped unexpectedly"))?
            .read_range(range)
    }

    pub(crate) fn read_range_with_arrivals(
        &self,
        range: Range<usize>,
    ) -> std::io::Result<Vec<(StoreItem, u64)>> {
        self.index
            .lock()
            .map_err(|_| std::io::Error::other("directory index worker stopped unexpectedly"))?
            .read_range_with_arrivals(range)
    }

    pub(crate) fn lookup_id(
        &self,
        id: &musheen_core::ItemId,
    ) -> std::io::Result<Option<StoreItem>> {
        self.index
            .lock()
            .map_err(|_| std::io::Error::other("directory index worker stopped unexpectedly"))?
            .lookup_id(id)
    }

    pub(crate) fn resolve_paths(
        &self,
        ids: &[musheen_core::ItemId],
    ) -> std::io::Result<Option<Vec<StorePath>>> {
        self.resolve_selection(ids)
            .map(|selection| selection.map(|(paths, _)| paths))
    }

    pub(crate) fn resolve_selection(
        &self,
        ids: &[musheen_core::ItemId],
    ) -> std::io::Result<Option<(Vec<StorePath>, Option<StoreItem>)>> {
        let mut index = self
            .index
            .lock()
            .map_err(|_| std::io::Error::other("directory index worker stopped unexpectedly"))?;
        let mut paths = Vec::with_capacity(ids.len());
        let mut first_item = None;
        for id in ids {
            let Some(item) = index.lookup_id(id)? else {
                return Ok(None);
            };
            paths.push(item.path().clone());
            if first_item.is_none() {
                first_item = Some(item);
            }
        }
        Ok(Some((paths, first_item)))
    }

    pub(crate) fn select_visible_range(
        &self,
        range: Range<usize>,
    ) -> std::io::Result<IndexedSelection> {
        self.index
            .lock()
            .map_err(|_| std::io::Error::other("directory index worker stopped unexpectedly"))?
            .selection_bitmap(range)
    }

    pub(crate) fn select_between_ids(
        &self,
        anchor: Option<&ItemId>,
        end: &ItemId,
    ) -> std::io::Result<Option<IndexedSelection>> {
        let mut index = self
            .index
            .lock()
            .map_err(|_| std::io::Error::other("directory index worker stopped unexpectedly"))?;
        let Some(end) = index.position_of_id(end)? else {
            return Ok(None);
        };
        let start = anchor
            .map(|id| index.position_of_id(id))
            .transpose()?
            .flatten()
            .unwrap_or(end);
        index
            .selection_bitmap(start.min(end)..start.max(end) + 1)
            .map(Some)
    }

    pub(crate) fn toggle_selection(
        &self,
        base: Option<IndexedSelection>,
        base_ids: &[ItemId],
        id: &ItemId,
    ) -> std::io::Result<Option<IndexedSelection>> {
        self.index
            .lock()
            .map_err(|_| std::io::Error::other("directory index worker stopped unexpectedly"))?
            .toggle_selection(base, base_ids, id)
    }

    pub(crate) fn rubber_band_selection(
        &self,
        base: Option<IndexedSelection>,
        base_ids: &[ItemId],
        positions: &[usize],
        mode: crate::views::SelectionMode,
    ) -> std::io::Result<IndexedSelection> {
        self.index
            .lock()
            .map_err(|_| std::io::Error::other("directory index worker stopped unexpectedly"))?
            .rubber_band_selection(base, base_ids, positions, mode)
    }

    pub(crate) fn move_focus(
        &self,
        current: Option<&ItemId>,
        anchor: Option<&ItemId>,
        delta: isize,
        extend: bool,
    ) -> std::io::Result<Option<IndexedFocusMove>> {
        let mut index = self
            .index
            .lock()
            .map_err(|_| std::io::Error::other("directory index worker stopped unexpectedly"))?;
        let count = index.visible_count().unwrap_or(0);
        if count == 0 {
            return Ok(None);
        }
        let current_position = current
            .map(|id| index.position_of_id(id))
            .transpose()?
            .flatten();
        let position = current_position.map_or_else(
            || if delta < 0 { count - 1 } else { 0 },
            |current| {
                if delta < 0 {
                    current.saturating_sub(delta.unsigned_abs())
                } else {
                    current.saturating_add(delta as usize).min(count - 1)
                }
            },
        );
        let id = index
            .read_range_with_arrivals(position..position + 1)?
            .pop()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "focused row is missing")
            })?
            .0
            .id()
            .clone();
        let selection = if extend {
            let start = anchor
                .map(|id| index.position_of_id(id))
                .transpose()?
                .flatten()
                .unwrap_or(position);
            Some(index.selection_bitmap(start.min(position)..start.max(position) + 1)?)
        } else {
            None
        };
        Ok(Some(IndexedFocusMove {
            id,
            position,
            selection,
        }))
    }

    pub(crate) fn resolve_bitmap(
        &self,
        selection: &IndexedSelection,
    ) -> std::io::Result<ResolvedIndexedSelection> {
        self.index
            .lock()
            .map_err(|_| std::io::Error::other("directory index worker stopped unexpectedly"))?
            .resolve_bitmap(selection)
    }

    pub(crate) fn resolve_command_targets(
        &self,
        ids: &[ItemId],
        bitmap: Option<&IndexedSelection>,
    ) -> std::io::Result<Option<(Vec<CommandTargetRef>, Option<StoreItem>)>> {
        let selected = if let Some(bitmap) = bitmap {
            let resolved = self.resolve_bitmap(bitmap)?;
            Some((resolved.targets, resolved.first_item))
        } else {
            self.resolve_selection(ids)?
                .map(|(paths, first_item)| (ids.iter().cloned().zip(paths).collect(), first_item))
        };
        selected
            .map(|(pairs, first_item)| {
                let targets = pairs
                    .into_iter()
                    .map(|(id, path)| {
                        CommandTargetRef::new(id, path)
                            .map_err(|error| std::io::Error::other(error.to_string()))
                    })
                    .collect::<std::io::Result<Vec<_>>>()?;
                Ok((targets, first_item))
            })
            .transpose()
    }

    pub(crate) fn resolve_focused_context_targets(
        &self,
        focused: &ItemId,
        selected_ids: &[ItemId],
        bitmap: Option<&IndexedSelection>,
    ) -> std::io::Result<Option<(Vec<CommandTargetRef>, Option<StoreItem>)>> {
        let Some((item, arrival)) = self
            .index
            .lock()
            .map_err(|_| std::io::Error::other("directory index worker stopped unexpectedly"))?
            .lookup_id_with_arrival(focused)?
        else {
            return Ok(None);
        };
        if bitmap.is_some_and(|bitmap| bitmap.contains(arrival)) {
            return self.resolve_command_targets(&[], bitmap);
        }
        if selected_ids.contains(focused) {
            return self.resolve_command_targets(selected_ids, None);
        }
        let target = CommandTargetRef::new(item.id().clone(), item.path().clone())
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Some((vec![target], Some(item))))
    }
}

pub(crate) struct DirectoryIndexWork {
    index: Option<SharedIndex>,
    index_root: Option<PathBuf>,
    prior_items: Vec<StoreItem>,
    page_items: Vec<StoreItem>,
    preferences: ViewPreferences,
    filter: Option<DirectoryFilter>,
    next_request: Option<PageRequest>,
}

pub(crate) struct DirectoryIndexResult {
    index: SharedIndex,
    indexed_count: usize,
    visible_count: usize,
    next_request: Option<PageRequest>,
    order_rebuilt: bool,
}

pub(crate) struct DirectoryIndexWatchWork {
    index: SharedIndex,
    /// The changes one merge applies; the changes that arrived while the
    /// previous merge ran travel together.
    events: Vec<WatchEvent>,
    preferences: ViewPreferences,
    filter: Option<DirectoryFilter>,
}

pub(crate) struct DirectoryIndexWatchResult {
    indexed_count: usize,
    visible_count: usize,
    removed: Vec<musheen_core::ItemId>,
    selection_transitions: Vec<(u64, Option<u64>)>,
}

pub(crate) struct DirectoryIndexOrderWork {
    index: SharedIndex,
    preferences: ViewPreferences,
    filter: Option<DirectoryFilter>,
    token: Arc<AtomicU64>,
    revision: u64,
}

impl DirectoryIndexOrderWork {
    pub(crate) fn run(self) -> std::io::Result<Option<(usize, usize)>> {
        let mut index = self
            .index
            .lock()
            .map_err(|_| std::io::Error::other("directory index worker stopped unexpectedly"))?;
        if self.token.load(Ordering::Acquire) != self.revision {
            return Ok(None);
        }
        index.rebuild_order(&self.preferences, self.filter.as_ref())?;
        Ok(Some((
            index.active_count().unwrap_or(0),
            index.visible_count().unwrap_or(0),
        )))
    }
}

impl DirectoryIndexWatchWork {
    pub(crate) fn run(self) -> std::io::Result<DirectoryIndexWatchResult> {
        let mut index = self
            .index
            .lock()
            .map_err(|_| std::io::Error::other("directory index worker stopped unexpectedly"))?;
        let mut removed = Vec::new();
        let mut selection_transitions = Vec::new();
        for event in self.events {
            let arrival = index.record_count();
            let event_id = match &event {
                WatchEvent::Created(item)
                | WatchEvent::Changed(item)
                | WatchEvent::Renamed { item, .. } => Some(item.id()),
                WatchEvent::Removed(id) => Some(id),
                WatchEvent::Invalidated { .. } => None,
            };
            let previous_arrival = event_id
                .map(|id| index.lookup_id_with_arrival(id))
                .transpose()?
                .flatten()
                .map(|(_, arrival)| arrival);
            let removed_id = match event {
                WatchEvent::Created(item)
                | WatchEvent::Changed(item)
                | WatchEvent::Renamed { item, .. } => {
                    index.append(&item, arrival)?;
                    None
                }
                WatchEvent::Removed(id) => {
                    index.append_tombstone(&id, arrival)?;
                    Some(id)
                }
                WatchEvent::Invalidated { .. } => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "invalidated directory must be reloaded",
                    ));
                }
            };
            index.rebuild_order(&self.preferences, self.filter.as_ref())?;
            if let Some(old) = previous_arrival {
                selection_transitions.push((
                    old,
                    if removed_id.is_some() {
                        None
                    } else {
                        Some(arrival)
                    },
                ));
            }
            removed.extend(removed_id);
        }
        Ok(DirectoryIndexWatchResult {
            indexed_count: index.active_count().unwrap_or(0),
            visible_count: index.visible_count().unwrap_or(0),
            removed,
            selection_transitions,
        })
    }
}

impl DirectoryIndexWork {
    pub(crate) fn with_filter(mut self, filter: Option<DirectoryFilter>) -> Self {
        self.filter = filter;
        self
    }

    pub(crate) fn run(self) -> std::io::Result<DirectoryIndexResult> {
        let index = match self.index {
            Some(index) => index,
            None => {
                let root = self.index_root.as_deref().ok_or_else(missing_index_root_error)?;
                Arc::new(Mutex::new(DiskDirectoryIndex::new_in(root)?))
            }
        };
        let (indexed_count, visible_count, order_rebuilt) = {
            let mut index_guard = index.lock().map_err(|_| {
                std::io::Error::other("directory index worker stopped unexpectedly")
            })?;
            for item in self.prior_items.into_iter().chain(self.page_items) {
                let arrival = index_guard.record_count();
                index_guard.append(&item, arrival)?;
            }
            let order_rebuilt =
                self.next_request.is_none() || index_guard.visible_count().is_none();
            if order_rebuilt {
                index_guard.rebuild_order(&self.preferences, self.filter.as_ref())?;
            }
            let count = usize::try_from(index_guard.record_count()).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "directory count overflow")
            })?;
            (
                count,
                index_guard.visible_count().unwrap_or(0),
                order_rebuilt,
            )
        };
        Ok(DirectoryIndexResult {
            index,
            indexed_count,
            visible_count,
            next_request: self.next_request,
            order_rebuilt,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DirectoryState {
    Loading,
    Empty,
    Ready,
    Error(Box<str>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyPageResult {
    Applied,
    Stale,
    Failed,
}

#[derive(Clone, Debug)]
pub struct DirectoryLoad {
    generation: u64,
    location: StorePath,
    cancellation: CancellationToken,
}

impl DirectoryLoad {
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn location(&self) -> &StorePath {
        &self.location
    }

    #[must_use]
    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}

#[derive(Debug)]
pub struct DirectoryModel {
    limits: ResourceLimits,
    generation: u64,
    order_epoch: u64,
    order_token: Arc<AtomicU64>,
    active: Option<DirectoryLoad>,
    state: DirectoryState,
    view: DirectoryViewModel,
    /// The most items this folder keeps in memory before it spills to its
    /// index. The Columns layout lowers it by the rows its parent columns
    /// hold, so a tab never holds more than `MAX_RESIDENT_ITEMS` models.
    resident_limit: usize,
    /// Where this tab's index lives; `None` when no cache directory is
    /// available, so a folder that needs an index reports the error.
    index_root: Option<PathBuf>,
    index: Option<SharedIndex>,
    /// Why the index stopped: the folder keeps what it already shows.
    index_error: Option<Box<str>>,
    indexed_selection: Option<IndexedSelection>,
    indexed_selection_anchor: Option<ItemId>,
    indexed_selection_epoch: u64,
    indexed_count: usize,
    indexed_visible_count: usize,
    next_request: Option<PageRequest>,
    page_loading: bool,
}

impl DirectoryModel {
    #[must_use]
    pub fn new(limits: ResourceLimits) -> Self {
        let retention_limit = limits.directory_retained_items();
        Self::new_with_retention(limits, retention_limit)
    }

    /// Creates a directory model with a bounded resident window. The requested
    /// retention is clamped to 4,096; larger directories spill to disk.
    #[must_use]
    pub fn new_with_retention(limits: ResourceLimits, retention_limit: usize) -> Self {
        Self {
            limits: limits.snapshot(),
            generation: 0,
            order_epoch: 0,
            order_token: Arc::new(AtomicU64::new(0)),
            active: None,
            state: DirectoryState::Empty,
            view: DirectoryViewModel::new(retention_limit.min(MAX_RESIDENT_ITEMS)),
            resident_limit: MAX_RESIDENT_ITEMS,
            index_root: directory_index_root(),
            index: None,
            index_error: None,
            indexed_selection: None,
            indexed_selection_anchor: None,
            indexed_selection_epoch: 0,
            indexed_count: 0,
            indexed_visible_count: 0,
            next_request: None,
            page_loading: false,
        }
    }

    /// Keeps this tab's disk index under `root` instead of the user's cache
    /// directory. `None` means no cache directory is available: a folder
    /// that needs an index keeps what it shows and reports the error.
    #[must_use]
    pub fn with_index_root(mut self, root: Option<PathBuf>) -> Self {
        self.index_root = root;
        self
    }

    #[cfg(test)]
    pub(crate) fn set_index_root(&mut self, root: Option<PathBuf>) {
        self.index_root = root;
    }

    /// The load this folder's watch events belong to.
    #[cfg(test)]
    pub(crate) fn current_load(&self) -> Option<DirectoryLoad> {
        self.active.clone()
    }

    /// The directory that holds this tab's index files while the folder is
    /// indexed.
    #[cfg(test)]
    pub(crate) fn index_directory(&self) -> Option<PathBuf> {
        self.index
            .as_ref()
            .and_then(|index| index.lock().ok().map(|index| index.path().to_path_buf()))
    }

    pub fn begin_navigation(&mut self, location: StorePath) -> DirectoryLoad {
        if let Some(active) = &self.active {
            active.cancellation.cancel();
        }
        self.generation = self.generation.wrapping_add(1);
        self.order_epoch = self.order_epoch.wrapping_add(1);
        self.order_token.fetch_add(1, Ordering::AcqRel);
        self.view.reset_items();
        self.index = None;
        self.index_error = None;
        self.indexed_selection = None;
        self.indexed_selection_anchor = None;
        self.indexed_selection_epoch = self.indexed_selection_epoch.wrapping_add(1);
        self.indexed_count = 0;
        self.indexed_visible_count = 0;
        self.next_request = Some(PageRequest::first(&self.limits));
        self.page_loading = false;
        self.state = DirectoryState::Loading;
        let load = DirectoryLoad {
            generation: self.generation,
            location,
            cancellation: CancellationToken::new(),
        };
        self.active = Some(load.clone());
        load
    }

    pub fn cancel(&self) {
        if let Some(active) = &self.active {
            active.cancellation.cancel();
        }
    }

    #[must_use]
    pub fn location(&self) -> Option<&StorePath> {
        self.active.as_ref().map(DirectoryLoad::location)
    }

    /// Monotonically identifies the active directory load. Delayed work
    /// captures this value so it cannot replay after navigation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub(crate) const fn order_epoch(&self) -> u64 {
        self.order_epoch
    }

    pub(crate) fn active_load(&self) -> Option<DirectoryLoad> {
        self.active.clone()
    }

    #[must_use]
    pub fn state(&self) -> &DirectoryState {
        &self.state
    }

    #[must_use]
    pub fn items(&self) -> &[StoreItem] {
        self.view.items()
    }

    #[must_use]
    pub const fn view(&self) -> &DirectoryViewModel {
        &self.view
    }

    pub fn view_mut(&mut self) -> &mut DirectoryViewModel {
        &mut self.view
    }

    pub fn apply_page(&mut self, load: &DirectoryLoad, page: Page<StoreItem>) -> ApplyPageResult {
        if !self.is_current(load) {
            return ApplyPageResult::Stale;
        }
        if self.needs_index(&page) {
            let work = self.prepare_index_page(page);
            return self.finish_index_page(load, work.run());
        }
        let next_request = page.next_request();
        let complete = next_request.is_none();
        self.view.extend(page.into_items());
        self.next_request = next_request;
        self.page_loading = false;
        self.view.set_complete(complete);
        self.state = if self.view.items().is_empty() {
            DirectoryState::Empty
        } else {
            DirectoryState::Ready
        };
        ApplyPageResult::Applied
    }

    pub(crate) fn needs_index(&self, page: &Page<StoreItem>) -> bool {
        self.index.is_some()
            || self.view.items().len().saturating_add(page.items().len()) > self.resident_limit
    }

    /// Gives up `reserved` of the tab's item budget to models held elsewhere,
    /// the rows the Columns layout keeps for its parent columns, so this
    /// folder spills to its index that much sooner. Applies to the pages that
    /// arrive after the call.
    pub(crate) fn reserve_resident_items(&mut self, reserved: usize) {
        self.resident_limit = MAX_RESIDENT_ITEMS.saturating_sub(reserved).max(1);
    }

    /// The most items this folder keeps in memory before it spills, and the
    /// most rows its viewport may hold once it is indexed.
    #[must_use]
    pub(crate) fn resident_limit(&self) -> usize {
        self.resident_limit
    }

    pub(crate) fn prepare_index_page(&mut self, page: Page<StoreItem>) -> DirectoryIndexWork {
        let next_request = page.next_request();
        if self.index.is_none() {
            self.state = DirectoryState::Loading;
        }
        DirectoryIndexWork {
            index: self.index.clone(),
            index_root: self.index_root.clone(),
            // The resident items stay in the view until the index holds them,
            // so a failed first spill leaves them on screen.
            prior_items: if self.index.is_none() {
                self.view.items().to_vec()
            } else {
                Vec::new()
            },
            page_items: page.into_items(),
            preferences: self.view.preferences().clone(),
            filter: None,
            next_request,
        }
    }

    pub(crate) fn finish_index_page(
        &mut self,
        load: &DirectoryLoad,
        result: std::io::Result<DirectoryIndexResult>,
    ) -> ApplyPageResult {
        if !self.is_current(load) {
            return ApplyPageResult::Stale;
        }
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                let message: Box<str> = format!("Directory index failed: {error}").into();
                self.next_request = None;
                self.page_loading = false;
                let shown = if self.index.is_some() {
                    self.indexed_count
                } else {
                    self.view.items().len()
                };
                if shown == 0 {
                    self.state = DirectoryState::Error(message);
                } else {
                    // The items already shown stay; the rest of the folder is
                    // not loaded and the count is reported as partial.
                    self.view.set_complete(false);
                    self.state = DirectoryState::Ready;
                    self.index_error = Some(message);
                }
                return ApplyPageResult::Failed;
            }
        };
        self.index = Some(result.index);
        self.view.take_items_for_index();
        if result.order_rebuilt {
            self.order_epoch = self.order_epoch.wrapping_add(1);
        }
        self.indexed_count = result.indexed_count;
        self.indexed_visible_count = result.visible_count;
        self.next_request = result.next_request;
        self.page_loading = false;
        self.view.set_complete(self.next_request.is_none());
        self.state = if self.indexed_count == 0 {
            DirectoryState::Empty
        } else {
            DirectoryState::Ready
        };
        ApplyPageResult::Applied
    }

    /// The error that stopped indexing while the folder keeps showing what it
    /// had loaded.
    #[must_use]
    pub fn index_error(&self) -> Option<&str> {
        self.index_error.as_deref()
    }

    #[must_use]
    pub fn indexed_count(&self) -> usize {
        self.index
            .as_ref()
            .map_or_else(|| self.view.items().len(), |_| self.indexed_count)
    }

    #[must_use]
    pub fn visible_count(&self) -> usize {
        self.index
            .as_ref()
            .map_or_else(|| self.view.visible_count(), |_| self.indexed_visible_count)
    }

    #[must_use]
    pub(crate) fn is_indexed(&self) -> bool {
        self.index.is_some()
    }

    pub(crate) fn set_indexed_selection(
        &mut self,
        selection: IndexedSelection,
        anchor: Option<ItemId>,
    ) {
        self.view.clear_selection();
        self.indexed_selection = Some(selection);
        self.indexed_selection_anchor = anchor;
        self.indexed_selection_epoch = self.indexed_selection_epoch.wrapping_add(1);
    }

    pub(crate) fn clear_indexed_selection(&mut self) {
        self.indexed_selection = None;
        self.indexed_selection_anchor = None;
        self.indexed_selection_epoch = self.indexed_selection_epoch.wrapping_add(1);
    }

    pub(crate) fn indexed_selection_epoch(&self) -> u64 {
        self.indexed_selection_epoch
    }

    pub(crate) fn indexed_selected_count(&self) -> usize {
        self.indexed_selection
            .as_ref()
            .map_or(0, IndexedSelection::count)
    }

    pub(crate) fn indexed_selection(&self) -> Option<&IndexedSelection> {
        self.indexed_selection.as_ref()
    }

    pub(crate) fn indexed_selection_anchor(&self) -> Option<&ItemId> {
        self.indexed_selection_anchor.as_ref()
    }

    pub(crate) fn is_indexed_arrival_selected(&self, arrival: u64) -> bool {
        self.indexed_selection
            .as_ref()
            .is_some_and(|selection| selection.contains(arrival))
    }

    pub(crate) fn index_reader(&self) -> Option<DirectoryIndexReader> {
        self.index.as_ref().map(|index| DirectoryIndexReader {
            index: Arc::clone(index),
        })
    }

    pub(crate) fn prepare_index_order(
        &mut self,
        load: &DirectoryLoad,
        filter: Option<DirectoryFilter>,
    ) -> Option<(u64, DirectoryIndexOrderWork)> {
        if !self.is_current(load) {
            return None;
        }
        let index = Arc::clone(self.index.as_ref()?);
        let revision = self.order_token.fetch_add(1, Ordering::AcqRel) + 1;
        Some((
            revision,
            DirectoryIndexOrderWork {
                index,
                preferences: self.view.preferences().clone(),
                filter,
                token: Arc::clone(&self.order_token),
                revision,
            },
        ))
    }

    pub(crate) fn finish_index_order(
        &mut self,
        load: &DirectoryLoad,
        revision: u64,
        result: std::io::Result<Option<(usize, usize)>>,
    ) -> bool {
        if !self.is_current(load) || self.order_token.load(Ordering::Acquire) != revision {
            return false;
        }
        match result {
            Ok(Some((indexed_count, visible_count))) => {
                self.indexed_count = indexed_count;
                self.indexed_visible_count = visible_count;
                self.order_epoch = self.order_epoch.wrapping_add(1);
                self.state = if self.indexed_count == 0 {
                    DirectoryState::Empty
                } else {
                    DirectoryState::Ready
                };
            }
            Ok(None) => return false,
            Err(error) => {
                self.state =
                    DirectoryState::Error(format!("Directory index failed: {error}").into());
            }
        }
        true
    }

    pub fn indexed_range(
        &mut self,
        range: Range<usize>,
    ) -> std::io::Result<Option<Vec<StoreItem>>> {
        self.index
            .as_ref()
            .map(|index| {
                index
                    .lock()
                    .map_err(|_| {
                        std::io::Error::other("directory index worker stopped unexpectedly")
                    })?
                    .read_range(range)
            })
            .transpose()
    }

    pub fn indexed_item(
        &mut self,
        id: &musheen_core::ItemId,
    ) -> std::io::Result<Option<StoreItem>> {
        self.index
            .as_ref()
            .map(|index| {
                index
                    .lock()
                    .map_err(|_| {
                        std::io::Error::other("directory index worker stopped unexpectedly")
                    })?
                    .lookup_id(id)
            })
            .transpose()
            .map(Option::flatten)
    }

    pub(crate) fn prepare_index_watch_event(
        &self,
        load: &DirectoryLoad,
        event: WatchEvent,
        filter: Option<DirectoryFilter>,
    ) -> Option<DirectoryIndexWatchWork> {
        self.prepare_index_watch_events(load, vec![event], filter)
    }

    /// The work that merges a batch of external changes into this folder's
    /// index. `None` when the folder is not indexed, the load is stale, the
    /// batch is empty, or a change invalidates the folder, which needs a
    /// reload instead.
    pub(crate) fn prepare_index_watch_events(
        &self,
        load: &DirectoryLoad,
        events: Vec<WatchEvent>,
        filter: Option<DirectoryFilter>,
    ) -> Option<DirectoryIndexWatchWork> {
        if !self.is_current(load)
            || events.is_empty()
            || events
                .iter()
                .any(|event| matches!(event, WatchEvent::Invalidated { .. }))
        {
            return None;
        }
        Some(DirectoryIndexWatchWork {
            index: Arc::clone(self.index.as_ref()?),
            events,
            preferences: self.view.preferences().clone(),
            filter,
        })
    }

    pub(crate) fn finish_index_watch_event(
        &mut self,
        load: &DirectoryLoad,
        result: std::io::Result<DirectoryIndexWatchResult>,
    ) -> bool {
        if !self.is_current(load) {
            return false;
        }
        match result {
            Ok(result) => {
                if let Some(selection) = &mut self.indexed_selection {
                    for (old, new) in result.selection_transitions {
                        if let Err(error) = selection.carry_forward(old, new) {
                            self.state = DirectoryState::Error(
                                format!("Directory selection failed: {error}").into(),
                            );
                            return true;
                        }
                    }
                }
                self.order_epoch = self.order_epoch.wrapping_add(1);
                self.indexed_count = result.indexed_count;
                self.indexed_visible_count = result.visible_count;
                for id in result.removed {
                    self.view.apply_watch_event(WatchEvent::Removed(id));
                }
                self.state = if self.indexed_count == 0 {
                    DirectoryState::Empty
                } else {
                    DirectoryState::Ready
                };
            }
            Err(error) => {
                self.state =
                    DirectoryState::Error(format!("Directory index failed: {error}").into());
            }
        }
        true
    }

    pub fn begin_page(&mut self) -> Option<(DirectoryLoad, PageRequest)> {
        if self.page_loading {
            return None;
        }
        let load = self.active.clone()?;
        let request = self.next_request.clone()?;
        self.page_loading = true;
        Some((load, request))
    }

    pub fn page_failed(&mut self, load: &DirectoryLoad) {
        if self.is_current(load) {
            self.page_loading = false;
        }
    }

    pub fn apply_error(&mut self, load: &DirectoryLoad, message: impl Into<Box<str>>) -> bool {
        if !self.is_current(load) {
            return false;
        }
        self.state = DirectoryState::Error(message.into());
        true
    }

    pub fn apply_watch_event(&mut self, load: &DirectoryLoad, event: WatchEvent) -> bool {
        if !self.is_current(load) {
            return false;
        }
        self.view.apply_watch_event(event);
        self.state = if self.view.items().is_empty() {
            DirectoryState::Empty
        } else {
            DirectoryState::Ready
        };
        true
    }

    #[must_use]
    pub fn rendered_range(&self, first_visible: usize, viewport_items: usize) -> Range<usize> {
        let item_count = self.visible_count();
        let start = first_visible.min(item_count);
        let rendered = viewport_items.saturating_mul(self.limits.directory_rendered_viewports());
        start..start.saturating_add(rendered).min(item_count)
    }

    pub(crate) fn is_current(&self, load: &DirectoryLoad) -> bool {
        self.active.as_ref().is_some_and(|active| {
            active.generation == load.generation
                && active.location == load.location
                && !load.cancellation.is_cancelled()
        })
    }
}

#[cfg(test)]
mod indexed_watch_tests {
    use super::*;
    use crate::search::DirectoryFilter;
    use crate::views::{SortDirection, SortKey};
    use musheen_core::{DisplayPath, ItemId, ItemKind, ProviderId, TotalHint};

    fn item(number: u64, name: &str) -> StoreItem {
        StoreItem::new(
            ItemId::new(ProviderId::new("local").unwrap(), number.to_be_bytes()).unwrap(),
            StorePath::from_unix_path(format!("/many/{name}")),
            DisplayPath::new(name),
            ItemKind::RegularFile,
            Some(number),
        )
    }

    #[test]
    fn indexed_folder_keeps_the_shown_items_when_the_index_cannot_be_written() {
        // A file where the index root should be makes every index write fail.
        let blocked = tempfile::NamedTempFile::new().unwrap();
        let mut model = DirectoryModel::new(ResourceLimits::default())
            .with_index_root(Some(blocked.path().to_path_buf()));
        let load = model.begin_navigation(StorePath::from_unix_path("/many"));
        let request = PageRequest::first(&ResourceLimits::default());

        for first in (0..4_608).step_by(512) {
            let items = (first..first + 512)
                .map(|number| item(number, &format!("item-{number:05}")))
                .collect();
            let page = Page::try_new(&request, items, None, TotalHint::Unknown).unwrap();
            let expected = if first < 4_096 {
                ApplyPageResult::Applied
            } else {
                ApplyPageResult::Failed
            };
            assert_eq!(model.apply_page(&load, page), expected, "page at {first}");
        }

        assert_eq!(
            model.view().items().len(),
            4_096,
            "the items already shown stay"
        );
        assert_eq!(model.state(), &DirectoryState::Ready);
        assert!(!model.is_indexed());
        assert!(
            !model.view().is_complete(),
            "the count is reported as partial"
        );
        assert!(
            model
                .index_error()
                .is_some_and(|error| error.contains("index")),
            "the error is available to show: {:?}",
            model.index_error()
        );
        assert!(
            model.begin_page().is_none(),
            "no further page is requested after the index failed"
        );
    }

    #[test]
    fn indexed_folder_reports_a_missing_cache_directory_and_keeps_the_shown_items() {
        let mut model = DirectoryModel::new(ResourceLimits::default()).with_index_root(None);
        let load = model.begin_navigation(StorePath::from_unix_path("/many"));
        let request = PageRequest::first(&ResourceLimits::default());

        for first in (0..4_608).step_by(512) {
            let items = (first..first + 512)
                .map(|number| item(number, &format!("item-{number:05}")))
                .collect();
            let page = Page::try_new(&request, items, None, TotalHint::Unknown).unwrap();
            let expected = if first < 4_096 {
                ApplyPageResult::Applied
            } else {
                ApplyPageResult::Failed
            };
            assert_eq!(model.apply_page(&load, page), expected, "page at {first}");
        }

        assert_eq!(model.view().items().len(), 4_096);
        assert_eq!(model.state(), &DirectoryState::Ready);
        assert!(!model.is_indexed(), "nothing is written anywhere else");
        assert!(!model.view().is_complete());
        assert!(
            model
                .index_error()
                .is_some_and(|error| error.contains("no cache directory")),
            "the error names the missing cache directory: {:?}",
            model.index_error()
        );
    }

    #[test]
    fn indexed_watch_updates_visible_order_and_preserves_selection() {
        let mut model = DirectoryModel::new(ResourceLimits::default());
        let load = model.begin_navigation(StorePath::from_unix_path("/many"));
        for batch in 0..9 {
            let request = PageRequest::first(&ResourceLimits::default());
            let items = (batch * 512..(batch + 1) * 512)
                .map(|number| item(number, &format!("item-{number}")))
                .collect();
            let page = Page::try_new(&request, items, None, TotalHint::Unknown).unwrap();
            assert_eq!(model.apply_page(&load, page), ApplyPageResult::Applied);
        }
        let id = item(0, "item-0").id().clone();
        let first_order = model.order_epoch();
        model
            .view_mut()
            .select_item(id.clone(), crate::views::SelectionMode::Replace);
        let renamed = item(0, "renamed");
        let work = model
            .prepare_index_watch_event(&load, WatchEvent::Changed(renamed.clone()), None)
            .unwrap();
        assert!(model.finish_index_watch_event(&load, work.run()));
        assert!(model.order_epoch() > first_order);
        assert_eq!(model.indexed_count(), 4_608);
        assert_eq!(model.indexed_item(&id).unwrap(), Some(renamed));
        assert_eq!(model.view().selected_ids(), std::slice::from_ref(&id));

        let work = model
            .prepare_index_watch_event(&load, WatchEvent::Removed(id.clone()), None)
            .unwrap();
        assert!(model.finish_index_watch_event(&load, work.run()));
        assert_eq!(model.indexed_count(), 4_607);
        assert!(model.indexed_item(&id).unwrap().is_none());
        assert!(model.view().selected_ids().is_empty());

        let offscreen = item(4_096, "item-4096").id().clone();
        let paths = model
            .index_reader()
            .unwrap()
            .resolve_paths(std::slice::from_ref(&offscreen))
            .unwrap()
            .unwrap();
        assert_eq!(paths, [StorePath::from_unix_path("/many/item-4096")]);
        let (paths, first_item) = model
            .index_reader()
            .unwrap()
            .resolve_selection(std::slice::from_ref(&offscreen))
            .unwrap()
            .unwrap();
        assert_eq!(paths, [StorePath::from_unix_path("/many/item-4096")]);
        assert_eq!(first_item.unwrap().id(), &offscreen);
        assert!(
            model
                .index_reader()
                .unwrap()
                .resolve_selection(&[id])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn indexed_bitmap_selection_follows_changed_identity_and_removal() {
        let mut model = DirectoryModel::new(ResourceLimits::default());
        let load = model.begin_navigation(StorePath::from_unix_path("/many"));
        for batch in 0..9 {
            let request = PageRequest::first(&ResourceLimits::default());
            let items = (batch * 512..(batch + 1) * 512)
                .map(|number| item(number, &format!("item-{number}")))
                .collect();
            let page = Page::try_new(&request, items, None, TotalHint::Unknown).unwrap();
            assert_eq!(model.apply_page(&load, page), ApplyPageResult::Applied);
        }
        let selection = model
            .index_reader()
            .unwrap()
            .select_visible_range(0..model.visible_count())
            .unwrap();
        model.set_indexed_selection(selection, None);
        assert_eq!(model.indexed_selected_count(), 4_608);

        let renamed = item(0, "renamed");
        let work = model
            .prepare_index_watch_event(&load, WatchEvent::Changed(renamed.clone()), None)
            .unwrap();
        assert!(model.finish_index_watch_event(&load, work.run()));
        let targets = model
            .index_reader()
            .unwrap()
            .resolve_command_targets(&[], model.indexed_selection())
            .unwrap()
            .unwrap()
            .0;
        assert_eq!(targets.len(), 4_608);
        assert!(targets.iter().any(|target| target.path() == renamed.path()));

        let work = model
            .prepare_index_watch_event(&load, WatchEvent::Removed(renamed.id().clone()), None)
            .unwrap();
        assert!(model.finish_index_watch_event(&load, work.run()));
        assert_eq!(model.indexed_selected_count(), 4_607);

        let work = model
            .prepare_index_watch_event(&load, WatchEvent::Created(item(9_000, "new")), None)
            .unwrap();
        assert!(model.finish_index_watch_event(&load, work.run()));
        assert_eq!(model.indexed_selected_count(), 4_607);
    }

    #[test]
    fn indexed_preferences_and_filter_rebuild_global_order() {
        let mut model = DirectoryModel::new(ResourceLimits::default());
        let load = model.begin_navigation(StorePath::from_unix_path("/many"));
        for batch in 0..9 {
            let request = PageRequest::first(&ResourceLimits::default());
            let items = (batch * 512..(batch + 1) * 512)
                .map(|number| {
                    let name = if number == 0 {
                        ".hidden".to_owned()
                    } else {
                        format!("item-{number}")
                    };
                    item(number, &name)
                })
                .collect();
            let page = Page::try_new(&request, items, None, TotalHint::Unknown).unwrap();
            assert_eq!(model.apply_page(&load, page), ApplyPageResult::Applied);
        }
        assert_eq!(model.visible_count(), 4_607);
        let old_epoch = model.order_epoch();
        model.view_mut().preferences_mut().show_hidden = true;
        model.view_mut().preferences_mut().sort.key = SortKey::Name;
        model.view_mut().preferences_mut().sort.direction = SortDirection::Descending;
        let (revision, work) = model.prepare_index_order(&load, None).unwrap();
        assert!(model.finish_index_order(&load, revision, work.run()));
        assert!(model.order_epoch() > old_epoch);
        assert_eq!(model.visible_count(), 4_608);
        assert_eq!(
            model.indexed_range(0..1).unwrap().unwrap()[0]
                .display_name()
                .as_str(),
            "item-4607"
        );

        let (revision, work) = model
            .prepare_index_order(&load, Some(DirectoryFilter::new("item-45")))
            .unwrap();
        assert!(model.finish_index_order(&load, revision, work.run()));
        assert_eq!(model.visible_count(), 111);
        assert_eq!(
            model.indexed_range(0..1).unwrap().unwrap()[0]
                .display_name()
                .as_str(),
            "item-4599"
        );
    }

    #[test]
    fn indexed_folder_merge_failure_keeps_the_shown_items_and_reports_the_error() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let mut model = DirectoryModel::new(ResourceLimits::default())
            .with_index_root(Some(root.path().to_path_buf()));
        let load = model.begin_navigation(StorePath::from_unix_path("/many"));
        for batch in 0..10 {
            let request = PageRequest::first(&ResourceLimits::default());
            let items = (batch * 512..(batch + 1) * 512)
                .map(|number| item(number, &format!("item-{number:05}")))
                .collect();
            let page = Page::try_new(&request, items, None, TotalHint::Unknown).unwrap();
            assert_eq!(model.apply_page(&load, page), ApplyPageResult::Applied);
        }
        assert!(model.is_indexed());
        assert_eq!(model.indexed_count(), 5_120);
        let scratch = model
            .index
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .path()
            .to_path_buf();

        // The index directory stops accepting new files, so the merge cannot
        // write the new order.
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o500)).unwrap();
        let work = model
            .prepare_index_watch_event(&load, WatchEvent::Created(item(9_000, "late")), None)
            .unwrap();
        let result = work.run();
        std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err(), "the merge fails in a read-only index directory");

        assert!(model.finish_index_watch_event(&load, result));
        assert_eq!(
            model.state(),
            &DirectoryState::Ready,
            "the items already shown stay on screen"
        );
        assert_eq!(model.indexed_count(), 5_120);
        assert!(
            model
                .index_error()
                .is_some_and(|error| error.contains("Directory index failed")),
            "the failed merge is reported: {:?}",
            model.index_error()
        );
    }
}

impl Default for DirectoryModel {
    fn default() -> Self {
        Self::new(ResourceLimits::default())
    }
}

pub async fn enumerate_directory(
    store: &dyn Store,
    load: &DirectoryLoad,
    limits: &ResourceLimits,
) -> Result<Vec<Page<StoreItem>>, StoreError> {
    let limits = limits.snapshot();
    let mut request = PageRequest::first(&limits);
    let mut pages = Vec::new();
    let mut retained = 0usize;

    loop {
        load.cancellation.check()?;
        let page = store
            .read_directory(&load.location, request, load.cancellation.clone())
            .await?;
        let next = page.next_request();
        retained = retained.saturating_add(page.items().len());
        pages.push(page);
        if retained >= limits.directory_retained_items() {
            break;
        }
        let Some(next) = next else {
            break;
        };
        request = next;
    }

    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::{ApplyPageResult, DirectoryModel};
    use musheen_core::{
        DisplayPath, ItemId, ItemKind, Page, PageRequest, ProviderId, ResourceLimits, StoreItem,
        StorePath, TotalHint,
    };

    fn page(first: usize) -> Page<StoreItem> {
        let request = PageRequest::first(&ResourceLimits::default());
        let provider = ProviderId::new("local").unwrap();
        let items = (first..first + 512)
            .map(|number| {
                StoreItem::new(
                    ItemId::new(provider.clone(), number.to_be_bytes()).unwrap(),
                    StorePath::from_unix_path(format!("/many/{number}")),
                    DisplayPath::new(number.to_string()),
                    ItemKind::RegularFile,
                    None,
                )
            })
            .collect();
        Page::try_new(&request, items, None, TotalHint::Unknown).unwrap()
    }

    #[test]
    fn background_index_work_publishes_ranges_only_for_current_navigation() {
        let mut model = DirectoryModel::default();
        let load = model.begin_navigation(StorePath::from_unix_path("/many"));
        for first in (0..4_096).step_by(512) {
            assert_eq!(
                model.apply_page(&load, page(first)),
                ApplyPageResult::Applied
            );
        }

        let work = model.prepare_index_page(page(4_096));
        let result = std::thread::spawn(move || work.run()).join().unwrap();
        assert_eq!(
            model.finish_index_page(&load, result),
            ApplyPageResult::Applied
        );
        assert_eq!(model.indexed_count(), 4_608);
        assert_eq!(
            model
                .index_reader()
                .unwrap()
                .read_range(4_096..4_097)
                .unwrap()[0]
                .display_name()
                .as_str(),
            "4096"
        );

        let stale_work = model.prepare_index_page(page(4_608));
        let next = model.begin_navigation(StorePath::from_unix_path("/other"));
        let stale_result = std::thread::spawn(move || stale_work.run()).join().unwrap();
        assert_eq!(
            model.finish_index_page(&load, stale_result),
            ApplyPageResult::Stale
        );
        assert_eq!(model.indexed_count(), 0);
        assert_eq!(model.location(), Some(next.location()));
    }

}
