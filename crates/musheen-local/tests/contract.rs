use futures_lite::future::block_on;
use musheen_core::{
    CancellationToken, CapabilityKind, CapabilityState, MutationRequest, PageRequest,
    ResourceLimits, Store, StoreError, StorePath,
};
use musheen_local::LocalStore;
use musheen_test_support::verify_read_only_provider;
use std::collections::HashSet;
use std::fs::File;

#[test]
fn local_directory_pages_are_bounded_complete_and_stably_identified() {
    let directory = tempfile::tempdir().expect("the temporary directory is created");
    for index in 0..520 {
        File::create(directory.path().join(format!("item-{index:04}")))
            .expect("the fixture file is created");
    }
    let store = LocalStore::new();
    let location = StorePath::from_unix_path(directory.path().as_os_str().to_os_string());
    let cancellation = CancellationToken::new();
    let mut request = PageRequest::first(&ResourceLimits::default());
    let mut identities = HashSet::new();
    let mut page_count = 0;

    loop {
        let page = block_on(store.read_directory(&location, request, cancellation.clone()))
            .expect("the page loads");
        assert!(page.items().len() <= 512);
        for item in page.items() {
            assert!(
                identities.insert(item.id().clone()),
                "item IDs must be unique"
            );
        }
        page_count += 1;
        let Some(next) = page.next_request() else {
            break;
        };
        request = next;
    }

    assert_eq!(page_count, 2);
    assert_eq!(identities.len(), 520);

    let repeated = block_on(store.read_directory(
        &location,
        PageRequest::first(&ResourceLimits::default()),
        CancellationToken::new(),
    ))
    .expect("the repeated page loads");
    assert!(
        repeated
            .items()
            .iter()
            .all(|item| identities.contains(item.id()))
    );
}

#[test]
fn local_capabilities_are_total_and_read_only_mutations_are_refused() {
    let directory = tempfile::tempdir().expect("the temporary directory is created");
    let store = LocalStore::new();
    let location = StorePath::from_unix_path(directory.path().as_os_str().to_os_string());
    let matrix = store.capabilities(&location);
    let filesystem = store
        .probe(&location)
        .expect("the filesystem probe succeeds");

    assert!(!filesystem.filesystem_type().is_empty());
    assert!(directory.path().starts_with(filesystem.mount_point()));

    for kind in CapabilityKind::ALL {
        assert!(
            matches!(matrix.get(kind), CapabilityState::Supported)
                || matrix.get(kind).reason().is_some(),
            "{kind:?} must have an explicit state"
        );
    }

    let result = block_on(store.mutate(MutationRequest::trash(location), CancellationToken::new()));
    assert!(matches!(result, Err(StoreError::Unsupported { .. })));
}

#[test]
fn cancelling_a_continuation_releases_its_open_directory() {
    let directory = tempfile::tempdir().expect("the temporary directory is created");
    for index in 0..513 {
        File::create(directory.path().join(format!("item-{index:04}")))
            .expect("the fixture file is created");
    }
    let store = LocalStore::new();
    let location = StorePath::from_unix_path(directory.path().as_os_str().to_os_string());
    let first = block_on(store.read_directory(
        &location,
        PageRequest::first(&ResourceLimits::default()),
        CancellationToken::new(),
    ))
    .expect("the first page loads");
    let continuation = first.next_request().expect("the second page exists");
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let cancelled = block_on(store.read_directory(&location, continuation.clone(), cancellation));
    assert!(matches!(cancelled, Err(StoreError::Cancelled)));

    let retried = block_on(store.read_directory(&location, continuation, CancellationToken::new()));
    assert!(matches!(retried, Err(StoreError::InvalidContinuation)));
}

#[test]
fn local_store_passes_the_shared_read_only_provider_contract() {
    let directory = tempfile::tempdir().expect("the temporary directory is created");
    File::create(directory.path().join("fixture")).expect("the fixture file is created");
    let store = LocalStore::new();
    let location = StorePath::from_unix_path(directory.path().as_os_str().to_os_string());

    block_on(verify_read_only_provider(&store, location))
        .expect("the local provider satisfies the shared contract");
}

#[test]
fn abandoned_enumerations_are_evicted_without_exceeding_the_descriptor_bound() {
    let directory = tempfile::tempdir().expect("the temporary directory is created");
    for index in 0..513 {
        File::create(directory.path().join(format!("item-{index:04}")))
            .expect("the fixture file is created");
    }
    let store = LocalStore::new();
    let location = StorePath::from_unix_path(directory.path().as_os_str().to_os_string());
    let mut continuations = Vec::new();
    for _ in 0..17 {
        let first = block_on(store.read_directory(
            &location,
            PageRequest::first(&ResourceLimits::default()),
            CancellationToken::new(),
        ))
        .expect("a bounded enumeration starts");
        continuations.push(first.next_request().expect("the second page exists"));
    }

    let oldest = continuations.remove(0);
    let result = block_on(store.read_directory(&location, oldest, CancellationToken::new()));
    assert!(matches!(result, Err(StoreError::InvalidContinuation)));

    for continuation in continuations {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let _ = block_on(store.read_directory(&location, continuation, cancellation));
    }
}
