use futures_lite::future::block_on;
use musheen_core::{CancellationToken, PageRequest, ResourceLimits, Store, StoreError, StorePath};
use musheen_local::{LocalStore, TraversalOptions};
use std::ffi::OsString;
use std::fs::{self, File, Permissions};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{PermissionsExt, symlink};

#[test]
fn non_utf8_case_distinct_and_sparse_entries_are_listed_losslessly() {
    let directory = tempfile::tempdir().expect("the temporary directory is created");
    let hostile_name = OsString::from_vec(b"bad-\xff-name".to_vec());
    File::create(directory.path().join(&hostile_name)).expect("the hostile file is created");
    File::create(directory.path().join("Case")).expect("the upper-case file is created");
    File::create(directory.path().join("case")).expect("the lower-case file is created");
    let sparse = File::create(directory.path().join("sparse")).expect("the sparse file is created");
    sparse
        .set_len(64 * 1024 * 1024)
        .expect("the sparse file is sized");

    let store = LocalStore::new();
    let location = StorePath::from_unix_path(directory.path().as_os_str().to_os_string());
    let page = block_on(store.read_directory(
        &location,
        PageRequest::first(&ResourceLimits::default()),
        CancellationToken::new(),
    ))
    .expect("the directory loads");

    let raw_names = page
        .items()
        .iter()
        .map(|item| {
            item.path()
                .as_unix_path()
                .expect("local items have Unix paths")
                .file_name()
                .expect("fixture items have names")
                .as_bytes()
                .to_vec()
        })
        .collect::<Vec<_>>();
    assert!(raw_names.contains(&b"bad-\xff-name".to_vec()));
    assert!(raw_names.contains(&b"Case".to_vec()));
    assert!(raw_names.contains(&b"case".to_vec()));
    assert_eq!(
        page.items()
            .iter()
            .find(|item| item.display_name().as_str() == "sparse")
            .expect("the sparse file is listed")
            .size(),
        Some(64 * 1024 * 1024)
    );
}

#[test]
fn unreadable_directory_returns_a_permission_error() {
    let root = tempfile::tempdir().expect("the temporary directory is created");
    let denied = root.path().join("denied");
    fs::create_dir(&denied).expect("the denied directory is created");
    fs::set_permissions(&denied, Permissions::from_mode(0o000)).expect("permissions are removed");

    let store = LocalStore::new();
    let location = StorePath::from_unix_path(denied.as_os_str().to_os_string());
    let result = block_on(store.read_directory(
        &location,
        PageRequest::first(&ResourceLimits::default()),
        CancellationToken::new(),
    ));

    fs::set_permissions(&denied, Permissions::from_mode(0o700))
        .expect("permissions are restored for cleanup");
    let running_as_root = fs::read_to_string("/proc/self/status")
        .expect("Linux process status is readable")
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|ids| ids.split_whitespace().nth(1))
        == Some("0");
    if running_as_root {
        assert!(
            result.is_ok(),
            "root should retain discretionary read access"
        );
        return;
    }
    assert!(matches!(
        result,
        Err(StoreError::Io {
            kind: std::io::ErrorKind::PermissionDenied,
            ..
        })
    ));
}

#[test]
fn traversal_does_not_follow_symlink_loops_by_default() {
    let root = tempfile::tempdir().expect("the temporary directory is created");
    let nested = root.path().join("nested");
    fs::create_dir(&nested).expect("the nested directory is created");
    File::create(nested.join("leaf")).expect("the leaf file is created");
    symlink(&nested, nested.join("loop")).expect("the loop symlink is created");

    let store = LocalStore::new();
    let location = StorePath::from_unix_path(root.path().as_os_str().to_os_string());
    let entries = store
        .traverse(&location, TraversalOptions::default())
        .expect("traversal starts")
        .collect::<Result<Vec<_>, _>>()
        .expect("default traversal terminates");

    assert!(entries.len() <= 4);
    assert_eq!(
        entries
            .iter()
            .filter(|item| item.display_name().as_str() == "leaf")
            .count(),
        1
    );

    let followed = store
        .traverse(
            &location,
            TraversalOptions {
                follow_symlinks: true,
                max_depth: 32,
                ..TraversalOptions::default()
            },
        )
        .expect("opt-in traversal starts")
        .take(16)
        .collect::<Vec<_>>();
    assert!(
        followed.iter().any(Result::is_err),
        "walkdir must report the symlink loop instead of following forever"
    );
}
