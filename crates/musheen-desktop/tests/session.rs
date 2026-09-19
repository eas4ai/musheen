use musheen_desktop::SessionStore;
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
