use crate::MillionItemFixture;
use musheen_core::{
    BoxFuture, CancellationToken, CapabilityMatrix, CapabilityReason, CapabilityState,
    Continuation, DirectoryWatch, DisplayPath, ItemId, ItemKind, MutationRequest, Page,
    PageRequest, ProviderId, Store, StoreError, StoreItem, StorePath, TotalHint, WatchEvent,
    WatchSemantics,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Default)]
pub struct RecordingMetrics {
    inner: Arc<RecordingMetricsInner>,
}

#[derive(Debug, Default)]
struct RecordingMetricsInner {
    page_requests: AtomicUsize,
    generated_items: AtomicUsize,
    largest_page: AtomicUsize,
    mutation_starts: AtomicUsize,
}

impl RecordingMetrics {
    #[must_use]
    pub fn page_requests(&self) -> usize {
        self.inner.page_requests.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn generated_items(&self) -> usize {
        self.inner.generated_items.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn largest_page(&self) -> usize {
        self.inner.largest_page.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn mutation_starts(&self) -> usize {
        self.inner.mutation_starts.load(Ordering::SeqCst)
    }
}

/// A deterministic provider that records bounded paging and mutation behavior.
#[derive(Debug)]
pub struct RecordingStore {
    provider: ProviderId,
    fixture: MillionItemFixture,
    capabilities: CapabilityMatrix,
    metrics: RecordingMetrics,
    watch_events: Arc<Mutex<VecDeque<WatchEvent>>>,
}

impl RecordingStore {
    #[must_use]
    pub fn read_only(fixture: MillionItemFixture) -> Self {
        let reason = CapabilityReason::new("recording store is read-only")
            .expect("the built-in capability reason is valid");
        Self {
            provider: ProviderId::new("recording.fixture")
                .expect("the built-in provider ID is valid"),
            fixture,
            capabilities: CapabilityMatrix::new(|_| CapabilityState::Unsupported(reason.clone())),
            metrics: RecordingMetrics::default(),
            watch_events: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    #[must_use]
    pub fn metrics(&self) -> RecordingMetrics {
        self.metrics.clone()
    }

    pub fn push_watch_event(&self, event: WatchEvent) -> Result<(), StoreError> {
        self.watch_events
            .lock()
            .map_err(|_| StoreError::Backend("recording watch queue is poisoned".into()))?
            .push_back(event);
        Ok(())
    }

    fn item(&self, index: usize) -> Result<StoreItem, StoreError> {
        let key = index.to_be_bytes().to_vec();
        let id = ItemId::new(self.provider.clone(), key.clone())
            .map_err(|error| StoreError::Backend(error.to_string().into()))?;
        let path = StorePath::from_provider_key(self.provider.clone(), key)
            .map_err(|error| StoreError::Backend(error.to_string().into()))?;
        Ok(StoreItem::new(
            id,
            path,
            DisplayPath::new(format!("item-{index}")),
            ItemKind::RegularFile,
            Some(index as u64),
        ))
    }
}

impl Store for RecordingStore {
    fn provider_id(&self) -> &ProviderId {
        &self.provider
    }

    fn capabilities(&self, _location: &StorePath) -> CapabilityMatrix {
        self.capabilities.clone()
    }

    fn read_directory<'a>(
        &'a self,
        _location: &'a StorePath,
        request: PageRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Page<StoreItem>, StoreError>> {
        Box::pin(async move {
            cancellation.check()?;
            self.fixture.delay(&cancellation).await?;
            self.metrics
                .inner
                .page_requests
                .fetch_add(1, Ordering::SeqCst);

            let start = request
                .continuation()
                .map(Continuation::decode_usize)
                .transpose()?
                .unwrap_or(0);
            if start > self.fixture.total_items() {
                return Err(StoreError::InvalidContinuation);
            }
            let end = start
                .saturating_add(request.page_size())
                .min(self.fixture.total_items());
            let items = (start..end)
                .map(|index| self.item(index))
                .collect::<Result<Vec<_>, _>>()?;
            self.metrics
                .inner
                .generated_items
                .fetch_add(items.len(), Ordering::SeqCst);
            self.metrics
                .inner
                .largest_page
                .fetch_max(items.len(), Ordering::SeqCst);
            let next = (end < self.fixture.total_items()).then(|| Continuation::from_usize(end));

            Page::try_new(
                &request,
                items,
                next,
                TotalHint::Exact(self.fixture.total_items() as u64),
            )
        })
    }

    fn watch_directory<'a>(
        &'a self,
        _location: &'a StorePath,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn DirectoryWatch>, StoreError>> {
        Box::pin(async move {
            cancellation.check()?;
            let watch: Box<dyn DirectoryWatch> = Box::new(RecordingWatch {
                events: Arc::clone(&self.watch_events),
            });
            Ok(watch)
        })
    }

    fn validate_mutation(&self, request: &MutationRequest) -> Result<(), StoreError> {
        Err(StoreError::unsupported(
            request.kind().as_str(),
            "recording store is read-only",
        ))
    }

    fn mutate<'a>(
        &'a self,
        request: MutationRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        let validation = cancellation
            .check()
            .and_then(|()| self.validate_mutation(&request));
        if let Err(error) = validation {
            return Box::pin(async move { Err(error) });
        }

        self.metrics
            .inner
            .mutation_starts
            .fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

struct RecordingWatch {
    events: Arc<Mutex<VecDeque<WatchEvent>>>,
}

impl DirectoryWatch for RecordingWatch {
    fn semantics(&self) -> WatchSemantics {
        WatchSemantics::ManualRefresh
    }

    fn next_event<'a>(
        &'a mut self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<WatchEvent, StoreError>> {
        Box::pin(async move {
            cancellation.check()?;
            self.events
                .lock()
                .map_err(|_| StoreError::Backend("recording watch queue is poisoned".into()))?
                .pop_front()
                .ok_or(StoreError::WatchEnded)
        })
    }
}
