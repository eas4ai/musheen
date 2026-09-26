use futures_lite::future::block_on;
use musheen_core::{
    CancellationToken, Continuation, MutationRequest, PageRequest, ResourceLimits, Store,
    StoreError, StorePath, WatchEvent, WatchFailure, WatchSemantics,
};
use musheen_test_support::{MillionItemFixture, RecordingStore, verify_read_only_provider};

#[test]
fn million_item_fixture_generates_only_requested_pages() {
    let fixture = MillionItemFixture::new(1_000_000).expect("the fixture size is valid");
    let store = RecordingStore::read_only(fixture);
    let location = StorePath::from_provider_key(store.provider_id().clone(), b"root".to_vec())
        .expect("the location is valid");
    let first_request = PageRequest::first(&ResourceLimits::default());
    let first = block_on(store.read_directory(&location, first_request, CancellationToken::new()))
        .expect("the first page loads");
    let second = block_on(store.read_directory(
        &location,
        first.next_request().expect("a second page exists"),
        CancellationToken::new(),
    ))
    .expect("the second page loads");
    let _third = block_on(store.read_directory(
        &location,
        second.next_request().expect("a third page exists"),
        CancellationToken::new(),
    ))
    .expect("the third page loads");

    let metrics = store.metrics();
    assert_eq!(metrics.page_requests(), 3);
    assert_eq!(metrics.generated_items(), 1_536);
    assert_eq!(metrics.largest_page(), 512);
}

#[test]
fn recording_store_refuses_work_before_recording_a_mutation_start() {
    let store =
        RecordingStore::read_only(MillionItemFixture::new(1).expect("the fixture size is valid"));
    let target = StorePath::from_provider_key(store.provider_id().clone(), b"item".to_vec())
        .expect("the target is valid");

    let result = block_on(store.mutate(MutationRequest::trash(target), CancellationToken::new()));

    assert!(matches!(result, Err(StoreError::Unsupported { .. })));
    assert_eq!(store.metrics().mutation_starts(), 0);
}

#[test]
fn recording_store_rejects_a_cursor_past_the_fixture_end() {
    let store =
        RecordingStore::read_only(MillionItemFixture::new(1).expect("the fixture size is valid"));
    let location = StorePath::from_provider_key(store.provider_id().clone(), b"root".to_vec())
        .expect("the location is valid");
    let request = PageRequest::new(512, Some(Continuation::from_usize(2)))
        .expect("the page request is valid");

    let result = block_on(store.read_directory(&location, request, CancellationToken::new()));

    assert!(matches!(result, Err(StoreError::InvalidContinuation)));
}

#[test]
fn recording_watch_reports_scripted_invalidation_and_then_ends() {
    let store =
        RecordingStore::read_only(MillionItemFixture::new(0).expect("the fixture size is valid"));
    let location = StorePath::from_provider_key(store.provider_id().clone(), b"root".to_vec())
        .expect("the location is valid");
    store
        .push_watch_event(WatchEvent::invalidation(
            location.clone(),
            WatchFailure::EventGap,
        ))
        .expect("the watch queue accepts the event");

    let mut watch = block_on(store.watch_directory(&location, CancellationToken::new()))
        .expect("the watch opens");
    assert_eq!(watch.semantics(), WatchSemantics::ManualRefresh);
    assert!(matches!(
        block_on(watch.next_event(CancellationToken::new())),
        Ok(WatchEvent::Invalidated {
            cause: WatchFailure::EventGap,
            ..
        })
    ));
    assert!(matches!(
        block_on(watch.next_event(CancellationToken::new())),
        Err(StoreError::WatchEnded)
    ));
}

#[test]
fn recording_store_passes_the_shared_provider_contract() {
    let store =
        RecordingStore::read_only(MillionItemFixture::new(513).expect("the fixture size is valid"));
    let location = StorePath::from_provider_key(store.provider_id().clone(), b"root".to_vec())
        .expect("the location is valid");

    block_on(verify_read_only_provider(&store, location))
        .expect("the recording provider satisfies the shared contract");
}
