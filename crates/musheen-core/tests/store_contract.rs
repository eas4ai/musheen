use futures_lite::future::block_on;
use musheen_core::{
    BoxFuture, CancellationToken, CapabilityKind, CapabilityMatrix, CapabilityReason,
    CapabilityState, Continuation, DirectoryWatch, DisplayPath, ItemId, ItemKind, MutationRequest,
    Page, PageRequest, PagingPolicy, ProviderId, ReconcileBuffer, ResourceLimitConfig,
    ResourceLimits, Store, StoreError, StoreItem, StorePath, TotalHint, WatchEvent, WatchFailure,
};
use std::sync::atomic::{AtomicUsize, Ordering};

const MILLION: usize = 1_000_000;

struct MillionItemStore {
    provider: ProviderId,
    page_requests: AtomicUsize,
    mutation_starts: AtomicUsize,
}

impl MillionItemStore {
    fn new() -> Self {
        Self {
            provider: ProviderId::new("million.fixture").expect("the provider ID is valid"),
            page_requests: AtomicUsize::new(0),
            mutation_starts: AtomicUsize::new(0),
        }
    }

    fn item(&self, index: usize) -> StoreItem {
        let key = index.to_be_bytes().to_vec();
        StoreItem::new(
            ItemId::new(self.provider.clone(), key.clone()).expect("the item ID is valid"),
            StorePath::from_provider_key(self.provider.clone(), key)
                .expect("the store path is valid"),
            DisplayPath::new(format!("item-{index}")),
            ItemKind::RegularFile,
            Some(index as u64),
        )
    }
}

impl Store for MillionItemStore {
    fn provider_id(&self) -> &ProviderId {
        &self.provider
    }

    fn capabilities(&self, _location: &StorePath) -> CapabilityMatrix {
        CapabilityMatrix::new(|_| {
            CapabilityState::Unsupported(
                CapabilityReason::new("fixture is read-only").expect("the reason is valid"),
            )
        })
    }

    fn read_directory<'a>(
        &'a self,
        _location: &'a StorePath,
        request: PageRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Page<StoreItem>, StoreError>> {
        Box::pin(async move {
            cancellation.check()?;
            self.page_requests.fetch_add(1, Ordering::SeqCst);

            let start = request
                .continuation()
                .map(Continuation::decode_usize)
                .transpose()?
                .unwrap_or(0);
            let end = start.saturating_add(request.page_size()).min(MILLION);
            let items = (start..end).map(|index| self.item(index)).collect();
            let next = (end < MILLION).then(|| Continuation::from_usize(end));

            Page::try_new(&request, items, next, TotalHint::Exact(MILLION as u64))
        })
    }

    fn watch_directory<'a>(
        &'a self,
        _location: &'a StorePath,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn DirectoryWatch>, StoreError>> {
        Box::pin(async {
            Err(StoreError::unsupported(
                "watch_directory",
                "fixture requires manual refresh",
            ))
        })
    }

    fn validate_mutation(&self, request: &MutationRequest) -> Result<(), StoreError> {
        Err(request.unsupported("fixture is read-only"))
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

        self.mutation_starts.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn paging_policy_captures_a_validated_limit_snapshot() {
    let limits = ResourceLimits::try_from(ResourceLimitConfig {
        directory_page_items: 64,
        directory_prefetch_pages: 1,
        directory_retained_items: 128,
        directory_rendered_viewports: 2,
    })
    .expect("the custom limits are valid");
    let policy = PagingPolicy::from(&limits);

    assert_eq!(policy.page_size(), 64);
    assert_eq!(policy.prefetch_pages(), 1);
    assert_eq!(policy.max_retained_items(), 128);
}

#[test]
fn paged_enumeration_is_stable_and_cancellable_between_pages() {
    let store = MillionItemStore::new();
    let location = StorePath::from_provider_key(store.provider.clone(), b"root".to_vec())
        .expect("the location is valid");
    let cancellation = CancellationToken::new();
    let first_request = PageRequest::first(&ResourceLimits::default());
    let first = block_on(store.read_directory(&location, first_request, cancellation.clone()))
        .expect("the first page loads");

    assert_eq!(first.items().len(), 512);
    assert_eq!(first.total_hint(), TotalHint::Exact(MILLION as u64));
    assert_eq!(store.page_requests.load(Ordering::SeqCst), 1);

    let repeated = block_on(store.read_directory(
        &location,
        PageRequest::first(&ResourceLimits::default()),
        CancellationToken::new(),
    ))
    .expect("the repeated page loads");
    assert_eq!(first.items()[0].id(), repeated.items()[0].id());

    let next_request = first.next_request().expect("a second page exists");
    cancellation.cancel();
    let cancelled = block_on(store.read_directory(&location, next_request, cancellation));

    assert!(matches!(cancelled, Err(StoreError::Cancelled)));
    assert_eq!(store.page_requests.load(Ordering::SeqCst), 2);
}

#[test]
fn unsupported_mutation_is_refused_before_work_starts() {
    let store = MillionItemStore::new();
    let target = StorePath::from_provider_key(store.provider.clone(), b"item".to_vec())
        .expect("the target is valid");

    assert!(matches!(
        store.capabilities(&target).get(CapabilityKind::Trash),
        CapabilityState::Unsupported(_)
    ));
    let result = block_on(store.mutate(MutationRequest::trash(target), CancellationToken::new()));

    assert!(matches!(result, Err(StoreError::Unsupported { .. })));
    assert_eq!(store.mutation_starts.load(Ordering::SeqCst), 0);
}

#[test]
fn overflow_invalidates_the_directory_and_reconciliation_deduplicates_ids() {
    let store = MillionItemStore::new();
    let location = StorePath::from_provider_key(store.provider.clone(), b"root".to_vec())
        .expect("the location is valid");
    let invalidation = WatchEvent::invalidation(location, WatchFailure::Overflow);
    assert!(matches!(
        invalidation,
        WatchEvent::Invalidated {
            cause: WatchFailure::Overflow,
            ..
        }
    ));

    let original = store.item(7);
    let replacement = StoreItem::new(
        original.id().clone(),
        original.path().clone(),
        DisplayPath::new("renamed-seven"),
        ItemKind::RegularFile,
        Some(7),
    );
    let mut reconciler = ReconcileBuffer::new(4_096).expect("the limit is valid");
    reconciler.apply(original).expect("the first item fits");
    reconciler.apply(replacement).expect("the replacement fits");

    assert_eq!(reconciler.len(), 1);
    assert_eq!(
        reconciler
            .get(store.item(7).id())
            .expect("the stable item remains")
            .display_name()
            .as_str(),
        "renamed-seven"
    );
}

#[test]
fn reconciliation_replaces_a_stale_identity_at_the_same_path() {
    let original = store_item(1);
    let replacement_identity = store_item(2).id().clone();
    let replacement = StoreItem::new(
        replacement_identity.clone(),
        original.path().clone(),
        DisplayPath::new("replacement"),
        ItemKind::RegularFile,
        Some(2),
    );
    let original_identity = original.id().clone();
    let mut reconciler = ReconcileBuffer::new(4).expect("the limit is valid");

    reconciler.apply(original).expect("the original item fits");
    reconciler
        .apply(replacement)
        .expect("the replacement item fits");

    assert_eq!(reconciler.len(), 1);
    assert!(reconciler.get(&original_identity).is_none());
    assert!(reconciler.get(&replacement_identity).is_some());
}

#[test]
fn store_trait_is_object_safe() {
    fn accepts_store(_store: &dyn Store) {}

    let store = MillionItemStore::new();
    accepts_store(&store);
}

#[test]
fn pages_and_reconciliation_refuse_their_memory_bounds() {
    let request = PageRequest::new(1, None).expect("the request is valid");
    let oversized = Page::try_new(
        &request,
        vec![store_item(1), store_item(2)],
        None,
        TotalHint::Unknown,
    );
    assert!(matches!(oversized, Err(StoreError::PageTooLarge { .. })));

    let mut reconciler = ReconcileBuffer::new(1).expect("the limit is valid");
    reconciler
        .apply(store_item(1))
        .expect("the first item fits");
    assert!(matches!(
        reconciler.apply(store_item(2)),
        Err(StoreError::ResourceLimit { .. })
    ));
}

fn store_item(index: usize) -> StoreItem {
    let provider = ProviderId::new("bounded.fixture").expect("the provider ID is valid");
    let key = index.to_be_bytes().to_vec();
    StoreItem::new(
        ItemId::new(provider.clone(), key.clone()).expect("the item ID is valid"),
        StorePath::from_provider_key(provider, key).expect("the store path is valid"),
        DisplayPath::new(format!("item-{index}")),
        ItemKind::RegularFile,
        None,
    )
}
