use futures_lite::future::block_on;
use musheen_core::{ResourceLimits, StorePath};
use musheen_test_support::{
    FaultCase, FaultCoverage, FaultPhase, FaultingReadStore, MillionItemFixture, RecordingStore,
};
use musheen_ui::{DirectoryModel, enumerate_directory};

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
