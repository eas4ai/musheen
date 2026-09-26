use futures_lite::future::block_on;
use musheen_core::{ResourceLimits, Store, StorePath};
use musheen_test_support::{
    FaultCase, FaultCoverage, FaultPhase, FaultingReadStore, MillionItemFixture, RecordingStore,
};
use musheen_ui::{ApplyPageResult, DirectoryModel, DirectoryState, enumerate_directory};

#[test]
fn matrix_names_missing_boundary_evidence() {
    let missing = FaultCase::new("journal.recovery", FaultPhase::Recovery);
    let mut coverage = FaultCoverage::new([
        FaultCase::new("provider.read_directory", FaultPhase::BeforeWork),
        missing,
    ]);
    coverage.cover(FaultCase::new(
        "provider.read_directory",
        FaultPhase::BeforeWork,
    ));

    assert_eq!(coverage.uncovered(), vec![missing]);
}

#[test]
fn paged_directory_failure_is_bounded_reported_and_retryable() {
    let boundary = "provider.read_directory";
    let mut coverage = FaultCoverage::new([
        FaultCase::new(boundary, FaultPhase::BeforeWork),
        FaultCase::new(boundary, FaultPhase::DuringPartialWork),
        FaultCase::new(boundary, FaultPhase::Recovery),
    ]);
    let limits = ResourceLimits::default();

    for (fail_on_call, phase) in [
        (0, FaultPhase::BeforeWork),
        (1, FaultPhase::DuringPartialWork),
    ] {
        let fixture = MillionItemFixture::new(600).unwrap().with_delay_yields(0);
        let store = FaultingReadStore::new(RecordingStore::read_only(fixture), fail_on_call);
        let metrics = store.metrics();
        let mut model = DirectoryModel::new(limits.clone());
        let load = model.begin_navigation(StorePath::from_unix_path("/fixture"));

        let failure = block_on(enumerate_directory(&store, &load, &limits)).unwrap_err();
        assert!(failure.to_string().contains("injected page read failure"));
        assert_eq!(store.read_calls(), fail_on_call + 1);
        assert_eq!(metrics.page_requests(), fail_on_call);
        assert!(metrics.generated_items() <= limits.directory_page_items());
        coverage.cover(FaultCase::new(boundary, phase));

        let pages = block_on(enumerate_directory(&store, &load, &limits)).unwrap();
        assert_eq!(
            pages.iter().map(|page| page.items().len()).sum::<usize>(),
            600
        );
        assert!(metrics.largest_page() <= limits.directory_page_items());
        assert_eq!(store.read_calls(), fail_on_call + 3);
        coverage.cover(FaultCase::new(boundary, FaultPhase::Recovery));
    }

    assert!(
        coverage.uncovered().is_empty(),
        "uncovered boundary IDs: {:?}",
        coverage.uncovered()
    );
}

#[test]
fn directory_page_failure_after_publication_preserves_items_and_retries() {
    let limits = ResourceLimits::default();
    let fixture = MillionItemFixture::new(600).unwrap().with_delay_yields(0);
    let store = FaultingReadStore::new(RecordingStore::read_only(fixture), 1);
    let mut model = DirectoryModel::new(limits.clone());
    let load = model.begin_navigation(StorePath::from_unix_path("/fixture"));

    let (_, first_request) = model.begin_page().unwrap();
    let first_page =
        block_on(store.read_directory(load.location(), first_request, load.cancellation().clone()))
            .unwrap();
    assert_eq!(
        model.apply_page(&load, first_page),
        ApplyPageResult::Applied
    );
    let published_count = model.items().len();
    assert_eq!(published_count, limits.directory_page_items());

    let (_, next_request) = model.begin_page().unwrap();
    let failure =
        block_on(store.read_directory(load.location(), next_request, load.cancellation().clone()))
            .unwrap_err();
    model.page_failed(&load);
    assert!(model.apply_error(&load, failure.to_string()));
    assert!(matches!(model.state(), DirectoryState::Error(_)));
    assert_eq!(model.items().len(), published_count);

    let (_, retry_request) = model.begin_page().unwrap();
    let retry_page =
        block_on(store.read_directory(load.location(), retry_request, load.cancellation().clone()))
            .unwrap();
    assert_eq!(
        model.apply_page(&load, retry_page),
        ApplyPageResult::Applied
    );
    assert_eq!(model.items().len(), 600);
    assert_eq!(model.state(), &DirectoryState::Ready);
    assert!(model.begin_page().is_none());
    assert_eq!(store.read_calls(), 3);
}
