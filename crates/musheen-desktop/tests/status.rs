use musheen_desktop::StatusStore;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn operation_status_documents_are_private_atomic_and_separate_from_sessions() {
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let store = StatusStore::from_config_home(temporary.path());

    assert!(store.path().ends_with("musheen/operations.json"));
    store.save(b"first").expect("first status document saves");
    store
        .save(b"second")
        .expect("replacement status document saves");

    assert_eq!(
        store.load().expect("status document loads"),
        Some(b"second".to_vec())
    );
    assert_eq!(
        store.load_backup().expect("status backup loads"),
        Some(b"first".to_vec())
    );
    assert_eq!(
        fs::metadata(store.path())
            .expect("status metadata is available")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}
