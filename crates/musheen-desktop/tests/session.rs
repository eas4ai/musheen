use musheen_desktop::{SessionStore, SessionStoreError};
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn session_documents_replace_atomically_with_private_permissions() {
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let path = temporary.path().join("nested/session.json");
    let store = SessionStore::at(&path);

    assert_eq!(store.load().expect("missing session is not an error"), None);
    let first = br#"{"schema_version":1,"windows":[{"name":"first"}]}"#;
    let second = br#"{"schema_version":1,"windows":[{"name":"second"}]}"#;
    store.save(first).expect("first session saves");
    store.save(second).expect("replacement session saves");

    assert_eq!(store.load().expect("session loads"), Some(second.to_vec()));
    assert_eq!(
        store.load_backup().expect("session backup loads"),
        Some(first.to_vec()),
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

    store
        .save(br#"{"schema_version":1,"windows":[{"name":"first"}]}"#)
        .expect("first session saves");

    assert_eq!(store.load_backup().expect("backup lookup succeeds"), None);
}

#[test]
fn damaged_primary_does_not_replace_the_last_good_session_backup() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("session.json");
    let store = SessionStore::at(&path);
    let first = br#"{"schema_version":1,"windows":[{"name":"first"}]}"#;
    let last_good = br#"{"schema_version":1,"windows":[{"name":"last-good"}]}"#;
    let next = br#"{"schema_version":1,"windows":[{"name":"next"}]}"#;

    store.save(first).unwrap();
    store.save(last_good).unwrap();
    fs::write(&path, b"{ interrupted").unwrap();

    store.save(next).unwrap();

    assert_eq!(store.load().unwrap().as_deref(), Some(next.as_slice()));
    assert_eq!(
        store.load_backup().unwrap().as_deref(),
        Some(first.as_slice())
    );
}

#[test]
fn malformed_session_shape_does_not_replace_the_last_good_backup() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("session.json");
    let store = SessionStore::at(&path);
    let first = br#"{"schema_version":1,"windows":[{"name":"first"}]}"#;
    let second = br#"{"schema_version":1,"windows":[{"name":"second"}]}"#;
    let next = br#"{"schema_version":1,"windows":[{"name":"next"}]}"#;
    store.save(first).unwrap();
    store.save(second).unwrap();
    fs::write(&path, br#"{"schema_version":1,"windows":"not an array"}"#).unwrap();

    store.save(next).unwrap();

    assert_eq!(
        store.load_backup().unwrap().as_deref(),
        Some(first.as_slice())
    );
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

#[test]
fn unrecognized_primary_schema_is_not_replaced_by_an_older_build() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("session.json");
    let store = SessionStore::at(&path);
    let newer = br#"{"schema_version":"next","windows":[],"future_field":true}"#;
    fs::write(&path, newer).unwrap();

    assert!(matches!(
        store.save(br#"{"schema_version":1,"windows":[]}"#),
        Err(SessionStoreError::UnrecognizedSchema { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), newer);
}

#[test]
fn unrecognized_backup_schema_is_not_replaced_by_an_older_build() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("session.json");
    let backup = temporary.path().join("session.json.bak");
    let store = SessionStore::at(&path);
    let older = br#"{"schema_version":1,"windows":[]}"#;
    let newer = br#"{"schema_version":"next","windows":[],"future_field":true}"#;
    fs::write(&path, older).unwrap();
    fs::write(&backup, newer).unwrap();

    assert!(matches!(
        store.save(older),
        Err(SessionStoreError::UnrecognizedSchema { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), older);
    assert_eq!(fs::read(&backup).unwrap(), newer);
}
