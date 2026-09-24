use crate::views::{DirectoryViewModel, ViewPreferences};
use musheen_core::{
    CancellationToken, Page, PageRequest, ResourceLimits, Store, StoreError, StoreItem, StorePath,
    WatchEvent,
};
use std::ops::Range;
use std::sync::{Arc, Mutex};

mod index;
use index::DiskDirectoryIndex;

const MAX_RESIDENT_ITEMS: usize = 4_096;

type SharedIndex = Arc<Mutex<DiskDirectoryIndex>>;

#[derive(Clone)]
pub(crate) struct DirectoryIndexReader {
    index: SharedIndex,
}

impl DirectoryIndexReader {
    pub(crate) fn read_range(&self, range: Range<usize>) -> std::io::Result<Vec<StoreItem>> {
        self.index
            .lock()
            .map_err(|_| std::io::Error::other("directory index worker stopped unexpectedly"))?
            .read_range(range)
    }
}

pub(crate) struct DirectoryIndexWork {
    index: Option<SharedIndex>,
    prior_items: Vec<StoreItem>,
    page_items: Vec<StoreItem>,
    preferences: ViewPreferences,
    next_request: Option<PageRequest>,
}

pub(crate) struct DirectoryIndexResult {
    index: SharedIndex,
    indexed_count: usize,
    visible_count: usize,
    next_request: Option<PageRequest>,
}

impl DirectoryIndexWork {
    pub(crate) fn run(self) -> std::io::Result<DirectoryIndexResult> {
        let index = match self.index {
            Some(index) => index,
            None => Arc::new(Mutex::new(DiskDirectoryIndex::new()?)),
        };
        let (indexed_count, visible_count) = {
            let mut index_guard = index.lock().map_err(|_| {
                std::io::Error::other("directory index worker stopped unexpectedly")
            })?;
            for item in self.prior_items.into_iter().chain(self.page_items) {
                let arrival = index_guard.record_count();
                index_guard.append(&item, arrival)?;
            }
            if self.next_request.is_none() || index_guard.visible_count().is_none() {
                index_guard.rebuild_order(&self.preferences, None)?;
            }
            let count = usize::try_from(index_guard.record_count()).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "directory count overflow")
            })?;
            (count, index_guard.visible_count().unwrap_or(0))
        };
        Ok(DirectoryIndexResult {
            index,
            indexed_count,
            visible_count,
            next_request: self.next_request,
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
    active: Option<DirectoryLoad>,
    state: DirectoryState,
    view: DirectoryViewModel,
    index: Option<SharedIndex>,
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
            active: None,
            state: DirectoryState::Empty,
            view: DirectoryViewModel::new(retention_limit.min(MAX_RESIDENT_ITEMS)),
            index: None,
            indexed_count: 0,
            indexed_visible_count: 0,
            next_request: None,
            page_loading: false,
        }
    }

    pub fn begin_navigation(&mut self, location: StorePath) -> DirectoryLoad {
        if let Some(active) = &self.active {
            active.cancellation.cancel();
        }
        self.generation = self.generation.wrapping_add(1);
        self.view.reset_items();
        self.index = None;
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
            || self.view.items().len().saturating_add(page.items().len()) > MAX_RESIDENT_ITEMS
    }

    pub(crate) fn prepare_index_page(&mut self, page: Page<StoreItem>) -> DirectoryIndexWork {
        let next_request = page.next_request();
        if self.index.is_none() {
            self.state = DirectoryState::Loading;
        }
        DirectoryIndexWork {
            index: self.index.clone(),
            prior_items: if self.index.is_none() {
                self.view.take_items_for_index()
            } else {
                Vec::new()
            },
            page_items: page.into_items(),
            preferences: self.view.preferences().clone(),
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
                self.state =
                    DirectoryState::Error(format!("Directory index failed: {error}").into());
                self.next_request = None;
                self.page_loading = false;
                return ApplyPageResult::Failed;
            }
        };
        self.index = Some(result.index);
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

    pub(crate) fn index_reader(&self) -> Option<DirectoryIndexReader> {
        self.index.as_ref().map(|index| DirectoryIndexReader {
            index: Arc::clone(index),
        })
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
