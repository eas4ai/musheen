use musheen_desktop::{SessionStore, SessionStoreError};
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn session_documents_replace_atomically_with_private_permissions() {
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let path = temporary.path().join("nested/session.json");
    let store = SessionStore::at(&path);

    assert_eq!(store.load().expect("missing session is not an error"), None);
    store.save(b"first").expect("first session saves");
    store.save(b"second").expect("replacement session saves");

    assert_eq!(
        store.load().expect("session loads"),
        Some(b"second".to_vec())
    );
    assert_eq!(
        store.load_backup().expect("session backup loads"),
        Some(b"first".to_vec()),
        "the previous valid document remains recoverable"
    );
    assert_eq!(
        fs::metadata(&path)
            .expect("session metadata is available")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::read_dir(path.parent().expect("session has a parent"))
            .expect("session directory is readable")
            .count(),
        2,
        "successful atomic replacement must not leave staging files"
    );
}

#[test]
fn first_session_write_does_not_fabricate_a_backup() {
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let store = SessionStore::at(temporary.path().join("session.json"));

    store.save(b"first").expect("first session saves");

    assert_eq!(store.load_backup().expect("backup lookup succeeds"), None);
}

#[test]
fn future_primary_session_is_not_replaced_by_an_older_build() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("session.json");
    let backup = temporary.path().join("session.json.bak");
    let store = SessionStore::at(&path);
    let future = br#"{"schema_version":2,"windows":[],"future_field":true}"#;
    let previous = br#"{"schema_version":1,"windows":[]}"#;
    fs::write(&path, future).unwrap();
    fs::write(&backup, previous).unwrap();

    assert!(matches!(
        store.save(previous),
        Err(SessionStoreError::FutureSchema { version: 2, .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), future);
    assert_eq!(fs::read(&backup).unwrap(), previous);
}

#[test]
fn future_backup_session_is_not_lost_after_fallback_restore() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("session.json");
    let backup = temporary.path().join("session.json.bak");
    let store = SessionStore::at(&path);
    let future = br#"{"schema_version":2,"windows":[],"future_field":true}"#;
    let previous = br#"{"schema_version":1,"windows":[]}"#;
    fs::write(&path, previous).unwrap();
    fs::write(&backup, future).unwrap();

    assert!(matches!(
        store.save(previous),
        Err(SessionStoreError::FutureSchema { version: 2, .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), previous);
    assert_eq!(fs::read(&backup).unwrap(), future);
}
