use crate::views::DirectoryViewModel;
use musheen_core::{
    CancellationToken, Page, PageRequest, ResourceLimits, Store, StoreError, StoreItem, StorePath,
    WatchEvent,
};
use std::ops::Range;

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
}

#[derive(Clone, Debug)]
pub struct DirectoryLoad {
    generation: u64,
    location: StorePath,
    cancellation: CancellationToken,
}

impl DirectoryLoad {
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
}

impl DirectoryModel {
    #[must_use]
    pub fn new(limits: ResourceLimits) -> Self {
        Self {
            limits: limits.snapshot(),
            generation: 0,
            active: None,
            state: DirectoryState::Empty,
            view: DirectoryViewModel::new(limits.directory_retained_items()),
        }
    }

    pub fn begin_navigation(&mut self, location: StorePath) -> DirectoryLoad {
        if let Some(active) = &self.active {
            active.cancellation.cancel();
        }
        self.generation = self.generation.wrapping_add(1);
        self.view.reset_items();
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

        let complete = page.next_request().is_none();
        self.view.extend(page.into_items());
        self.view.set_complete(complete);
        self.state = if self.view.items().is_empty() {
            DirectoryState::Empty
        } else {
            DirectoryState::Ready
        };
        ApplyPageResult::Applied
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
        let item_count = self.view.visible_count();
        let start = first_visible.min(item_count);
        let rendered = viewport_items.saturating_mul(self.limits.directory_rendered_viewports());
        start..start.saturating_add(rendered).min(item_count)
    }

    fn is_current(&self, load: &DirectoryLoad) -> bool {
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
