use futures_lite::future::block_on;
use musheen_core::{
    CancellationToken, PageRequest, ResourceLimits, Store, StoreError, StorePath, WatchEvent,
    WatchSemantics,
};
use musheen_local::LocalStore;
use std::fs::{self, File};
use std::thread;
use std::time::Duration;

#[test]
fn external_rename_keeps_the_stable_identity_or_invalidates_for_rescan() {
    let directory = tempfile::tempdir().expect("the temporary directory is created");
    let old_path = directory.path().join("before");
    let new_path = directory.path().join("after");
    File::create(&old_path).expect("the watched file is created");
    let store = LocalStore::new();
    let location = StorePath::from_unix_path(directory.path().as_os_str().to_os_string());
    let initial = block_on(store.read_directory(
        &location,
        PageRequest::first(&ResourceLimits::default()),
        CancellationToken::new(),
    ))
    .expect("the initial directory loads");
    let original_id = initial.items()[0].id().clone();
    let cancellation = CancellationToken::new();
    let cancel_later = cancellation.clone();
    let mut watch = block_on(store.watch_directory(&location, cancellation.clone()))
        .expect("the live watch opens");
    assert_eq!(watch.semantics(), WatchSemantics::Live);

    let rename_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        fs::rename(old_path, new_path).expect("the external rename succeeds");
    });
    let _timeout_thread = thread::spawn(move || {
        thread::sleep(Duration::from_secs(3));
        cancel_later.cancel();
    });

    let mut observed = None;
    for _ in 0..8 {
        match block_on(watch.next_event(cancellation.clone())) {
            Ok(WatchEvent::Renamed { item, .. }) | Ok(WatchEvent::Created(item)) => {
                observed = Some(item.id().clone());
                break;
            }
            Ok(WatchEvent::Invalidated { .. }) => break,
            Ok(_) => {}
            Err(StoreError::Cancelled) => break,
            Err(error) => panic!("watch failed before reporting the rename: {error}"),
        }
    }

    rename_thread.join().expect("the rename thread finishes");
    if let Some(observed_id) = observed {
        assert_eq!(observed_id, original_id);
    }
}

#[test]
fn cancellation_wakes_a_pending_watch() {
    let directory = tempfile::tempdir().expect("the temporary directory is created");
    let store = LocalStore::new();
    let location = StorePath::from_unix_path(directory.path().as_os_str().to_os_string());
    let cancellation = CancellationToken::new();
    let cancel_later = cancellation.clone();
    let mut watch = block_on(store.watch_directory(&location, cancellation.clone()))
        .expect("the live watch opens");
    let cancel_thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        cancel_later.cancel();
    });

    let result = block_on(watch.next_event(cancellation));

    cancel_thread
        .join()
        .expect("the cancellation thread finishes");
    assert!(matches!(result, Err(StoreError::Cancelled)));
}
