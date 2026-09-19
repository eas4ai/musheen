use musheen_core::{CancellationToken, StorePath};
use musheen_local::LocalStore;
use musheen_ops::{
    CopyProvider, CopyRequest, CopySession, EventGeneration, JobId, MetadataReport, MoveStrategy,
    StagingPath, execute_move,
};
use posix_acl::{PosixACL, Qualifier};
use rustix::fs::{AtFlags, CWD, Timespec, Timestamps, utimensat};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

#[test]
fn local_copy_preserves_bytes_mode_and_symbolic_links() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.bin");
    let destination = root.path().join("destination.bin");
    fs::write(&source, b"musheen-copy").unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();
    let preserved_time = Timespec {
        tv_sec: 1_650_000_000,
        tv_nsec: 987_654_321,
    };
    utimensat(
        CWD,
        &source,
        &Timestamps {
            last_access: preserved_time,
            last_modification: preserved_time,
        },
        AtFlags::empty(),
    )
    .unwrap();
    let mut provider = LocalStore::new();

    CopySession::default()
        .execute(
            &mut provider,
            &request(&source, &destination),
            &CancellationToken::new(),
        )
        .unwrap();

    let destination_metadata = fs::metadata(&destination).unwrap();
    assert_eq!(destination_metadata.mode() & 0o777, 0o640);
    assert_eq!(destination_metadata.atime(), preserved_time.tv_sec);
    assert_eq!(destination_metadata.atime_nsec(), preserved_time.tv_nsec);
    assert_eq!(destination_metadata.mtime(), preserved_time.tv_sec);
    assert_eq!(destination_metadata.mtime_nsec(), preserved_time.tv_nsec);
    assert_eq!(fs::read(&destination).unwrap(), b"musheen-copy");

    let link_source = root.path().join("source-link");
    let link_destination = root.path().join("destination-link");
    symlink("source.bin", &link_source).unwrap();
    CopySession::default()
        .execute(
            &mut provider,
            &request(&link_source, &link_destination),
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(
        fs::read_link(link_destination).unwrap(),
        source.file_name().unwrap()
    );
}

#[test]
fn local_move_uses_atomic_rename_and_preserves_the_only_valid_copy() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("move-source");
    let destination = root.path().join("move-destination");
    fs::write(&source, b"move-me").unwrap();
    let source_device = fs::metadata(&source).unwrap().dev();
    let mut provider = LocalStore::new();

    let outcome = execute_move(
        &mut provider,
        &request(&source, &destination),
        &CancellationToken::new(),
    )
    .unwrap();

    assert_eq!(outcome.strategy(), MoveStrategy::AtomicRename);
    assert!(!source.exists());
    assert_eq!(fs::read(&destination).unwrap(), b"move-me");
    assert_eq!(fs::metadata(destination).unwrap().dev(), source_device);
}

#[test]
fn local_directory_copy_finishes_children_before_restoring_read_only_mode() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source-tree");
    let nested = source.join("locked");
    let destination = root.path().join("destination-tree");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&nested).unwrap();
    fs::write(nested.join("child.txt"), b"still reachable").unwrap();
    fs::hard_link(nested.join("child.txt"), nested.join("second.txt")).unwrap();
    let preserved_time = Timespec {
        tv_sec: 1_600_000_000,
        tv_nsec: 123_456_789,
    };
    utimensat(
        CWD,
        &nested,
        &Timestamps {
            last_access: preserved_time,
            last_modification: preserved_time,
        },
        AtFlags::empty(),
    )
    .unwrap();
    fs::set_permissions(&nested, fs::Permissions::from_mode(0o555)).unwrap();
    let mut provider = LocalStore::new();
    let source_path = StorePath::from_unix_path(source.as_os_str());
    let destination_path = StorePath::from_unix_path(destination.as_os_str());
    let source_snapshot = provider.inspect(&source_path, false).unwrap();

    let outcome = CopySession::default()
        .execute(
            &mut provider,
            &request(&source, &destination),
            &CancellationToken::new(),
        )
        .unwrap();

    let copied = destination.join("locked");
    assert_eq!(
        fs::read(copied.join("child.txt")).unwrap(),
        b"still reachable"
    );
    assert_eq!(
        fs::metadata(copied.join("child.txt")).unwrap().ino(),
        fs::metadata(copied.join("second.txt")).unwrap().ino()
    );
    let copied_metadata = fs::metadata(&copied).unwrap();
    assert_eq!(copied_metadata.mode() & 0o777, 0o555);
    assert_eq!(copied_metadata.atime(), preserved_time.tv_sec);
    assert_eq!(copied_metadata.atime_nsec(), preserved_time.tv_nsec);
    assert_eq!(copied_metadata.mtime(), preserved_time.tv_sec);
    assert_eq!(copied_metadata.mtime_nsec(), preserved_time.tv_nsec);

    fs::set_permissions(&copied, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        !provider
            .verify(
                &source_path,
                &source_snapshot,
                &destination_path,
                outcome.metadata(),
            )
            .unwrap()
    );
}

#[test]
fn local_copy_and_move_never_replace_an_existing_destination() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("destination");
    fs::write(&source, b"new bytes").unwrap();
    fs::write(&destination, b"keep bytes").unwrap();
    let mut provider = LocalStore::new();

    assert!(
        CopySession::default()
            .execute(
                &mut provider,
                &request(&source, &destination),
                &CancellationToken::new(),
            )
            .is_err()
    );
    assert_eq!(fs::read(&source).unwrap(), b"new bytes");
    assert_eq!(fs::read(&destination).unwrap(), b"keep bytes");

    assert!(
        execute_move(
            &mut provider,
            &request(&source, &destination),
            &CancellationToken::new(),
        )
        .is_err()
    );
    assert_eq!(fs::read(source).unwrap(), b"new bytes");
    assert_eq!(fs::read(destination).unwrap(), b"keep bytes");
}

#[test]
fn local_copy_preserves_an_existing_recovery_staging_path() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("destination");
    fs::write(&source, b"new bytes").unwrap();
    let request = request(&source, &destination);
    let staging = StagingPath::for_destination(
        request.destination(),
        JobId::new(1).unwrap(),
        EventGeneration::new(0),
    )
    .unwrap();
    let staging_path = staging.path().as_unix_path().unwrap();
    fs::write(staging_path, b"recovery bytes").unwrap();
    let mut provider = LocalStore::new();

    assert!(
        CopySession::default()
            .execute(&mut provider, &request, &CancellationToken::new())
            .is_err()
    );

    assert_eq!(fs::read(staging_path).unwrap(), b"recovery bytes");
    assert!(!destination.exists());
}

#[test]
fn recovery_staging_can_be_discarded_only_through_an_app_owned_name() {
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("destination");
    let staging = StagingPath::for_destination(
        &StorePath::from_unix_path(destination.as_os_str()),
        JobId::new(9).unwrap(),
        EventGeneration::new(2),
    )
    .unwrap();
    fs::write(staging.path().as_unix_path().unwrap(), b"partial").unwrap();
    let unrelated = root.path().join("unrelated");
    fs::write(&unrelated, b"keep").unwrap();
    let mut provider = LocalStore::new();

    assert!(provider.recovery_staging_available(staging.path()));
    provider.discard_recovery_staging(staging.path()).unwrap();
    assert!(!provider.recovery_staging_available(staging.path()));
    assert!(!staging.path().as_unix_path().unwrap().exists());
    assert!(
        provider
            .discard_recovery_staging(&StorePath::from_unix_path(unrelated.as_os_str()))
            .is_err()
    );
    assert_eq!(fs::read(unrelated).unwrap(), b"keep");
}

#[test]
fn local_copy_preserves_extended_attributes_and_access_control_lists() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source-with-metadata");
    let destination = root.path().join("destination-with-metadata");
    fs::write(&source, b"metadata").unwrap();
    xattr::set(&source, "user.musheen-test", b"kept").unwrap();
    let mut source_acl = PosixACL::new(0o640);
    source_acl.set(Qualifier::User(123_456), 0o4);
    source_acl.write_acl(&source).unwrap();
    let expected_acl = PosixACL::read_acl(&source).unwrap();
    let mut provider = LocalStore::new();

    CopySession::default()
        .execute(
            &mut provider,
            &request(&source, &destination),
            &CancellationToken::new(),
        )
        .unwrap();

    assert_eq!(
        xattr::get(&destination, "user.musheen-test").unwrap(),
        Some(b"kept".to_vec())
    );
    assert_eq!(PosixACL::read_acl(destination).unwrap(), expected_acl);
}

#[test]
fn local_verification_rejects_metadata_corruption() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let destination = root.path().join("destination");
    fs::write(&source, b"same data").unwrap();
    fs::set_permissions(&source, fs::Permissions::from_mode(0o640)).unwrap();
    let source_path = StorePath::from_unix_path(source.as_os_str());
    let destination_path = StorePath::from_unix_path(destination.as_os_str());
    let mut provider = LocalStore::new();
    let source_snapshot = provider.inspect(&source_path, false).unwrap();
    CopySession::default()
        .execute(
            &mut provider,
            &request(&source, &destination),
            &CancellationToken::new(),
        )
        .unwrap();

    assert!(
        provider
            .verify(
                &source_path,
                &source_snapshot,
                &destination_path,
                &MetadataReport::default(),
            )
            .unwrap()
    );
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        !provider
            .verify(
                &source_path,
                &source_snapshot,
                &destination_path,
                &MetadataReport::default(),
            )
            .unwrap()
    );
}

#[test]
fn local_sparse_copy_preserves_holes_and_data_extents() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("sparse-source");
    let staging = root.path().join("sparse-staging");
    let logical_size = 8 * 1024 * 1024;
    let data_offset = 4 * 1024 * 1024;
    let mut source_file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&source)
        .unwrap();
    source_file.set_len(logical_size).unwrap();
    source_file.seek(SeekFrom::Start(data_offset)).unwrap();
    source_file.write_all(b"island").unwrap();
    source_file.sync_all().unwrap();
    drop(source_file);
    let source_path = StorePath::from_unix_path(source.as_os_str());
    let staging_path = StorePath::from_unix_path(staging.as_os_str());
    let mut provider = LocalStore::new();

    assert!(
        provider
            .try_sparse_copy(&source_path, &staging_path, &CancellationToken::new())
            .unwrap()
            .is_some()
    );

    let metadata = fs::metadata(&staging).unwrap();
    assert_eq!(metadata.len(), logical_size);
    assert!(metadata.blocks() * 512 < logical_size);
    let mut copied = fs::File::open(staging).unwrap();
    copied.seek(SeekFrom::Start(data_offset)).unwrap();
    let mut data = [0_u8; 6];
    copied.read_exact(&mut data).unwrap();
    assert_eq!(&data, b"island");
}

fn request(source: &std::path::Path, destination: &std::path::Path) -> CopyRequest {
    CopyRequest::new(
        JobId::new(1).unwrap(),
        EventGeneration::new(0),
        StorePath::from_unix_path(source.as_os_str()),
        StorePath::from_unix_path(destination.as_os_str()),
    )
}
