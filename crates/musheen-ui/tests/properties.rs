use musheen_core::{CancellationToken, ItemKind};
use musheen_desktop::{
    AggregateValue, ChecksumAlgorithm, ChecksumError, ChecksumService, PropertyRefresh,
    PropertySnapshot, RecursiveSize, XattrState,
};
use musheen_ui::{
    ApplicationChoice, OpenWithIntent, OpenWithModel, PropertiesDialogModel, PropertiesPage,
    PropertiesState,
};
use std::fs::{self, File};
use std::io::Write;

#[cfg(unix)]
#[test]
fn snapshots_cover_file_folder_symlink_xattrs_and_mixed_values() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let temporary = tempfile::tempdir().unwrap();
    let file = temporary.path().join("file.txt");
    let folder = temporary.path().join("folder");
    let link = temporary.path().join("link");
    fs::write(&file, b"hello").unwrap();
    fs::create_dir(&folder).unwrap();
    symlink(&file, &link).unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o750)).unwrap();
    xattr::set(&file, "user.musheen-test", b"value").unwrap();

    let snapshot = PropertySnapshot::load(&[file.clone(), folder.clone(), link]).unwrap();

    assert_eq!(snapshot.items().len(), 3);
    assert_eq!(snapshot.items()[0].kind(), ItemKind::RegularFile);
    assert_eq!(snapshot.items()[1].kind(), ItemKind::Directory);
    assert_eq!(snapshot.items()[2].kind(), ItemKind::SymbolicLink);
    assert_eq!(snapshot.items()[0].permissions().mode(), 0o640);
    assert!(matches!(
        snapshot.items()[0].xattrs(),
        XattrState::Available(values)
            if values.iter().any(|value| value.name() == "user.musheen-test")
    ));
    assert_eq!(snapshot.aggregate().kind(), AggregateValue::Mixed);
    assert_eq!(snapshot.aggregate().mode(), AggregateValue::Mixed);
    assert_eq!(snapshot.refresh_state().unwrap(), PropertyRefresh::Current);
}

#[cfg(unix)]
#[test]
fn a_broken_symlink_still_has_inspectable_properties() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().unwrap();
    let link = temporary.path().join("broken-link");
    symlink(temporary.path().join("missing-target"), &link).unwrap();

    let snapshot = PropertySnapshot::load(&[link]).unwrap();

    assert_eq!(snapshot.items()[0].kind(), ItemKind::SymbolicLink);
    assert_eq!(snapshot.items()[0].mime_type(), "inode/symlink");
    assert_eq!(snapshot.refresh_state().unwrap(), PropertyRefresh::Current);
}

#[test]
fn recursive_size_is_explicit_cancellable_and_sparse_aware() {
    let temporary = tempfile::tempdir().unwrap();
    let folder = temporary.path().join("folder");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("small"), b"12345").unwrap();
    File::create(folder.join("sparse"))
        .unwrap()
        .set_len(1_u64 << 40)
        .unwrap();

    let size = RecursiveSize::calculate(&folder, CancellationToken::new()).unwrap();
    assert_eq!(size.file_count(), 2);
    assert!(size.logical_bytes() >= 1_u64 << 40);
    assert!(size.allocated_bytes() < size.logical_bytes());

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(RecursiveSize::calculate(&folder, cancellation).is_err());
}

#[test]
fn checksums_are_streamed_known_answers_and_invalidated_by_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("input");
    fs::write(&path, b"abc").unwrap();

    let blake3 =
        ChecksumService::compute(&path, ChecksumAlgorithm::Blake3, CancellationToken::new())
            .unwrap();
    assert_eq!(
        blake3.hex_digest(),
        "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
    );
    let sha256 =
        ChecksumService::compute(&path, ChecksumAlgorithm::Sha256, CancellationToken::new())
            .unwrap();
    assert_eq!(
        sha256.hex_digest(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(sha256.is_current().unwrap());

    fs::remove_file(&path).unwrap();
    fs::write(&path, b"abc").unwrap();
    assert!(!sha256.is_current().unwrap());

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        ChecksumService::compute(&path, ChecksumAlgorithm::Sha256, cancellation),
        Err(ChecksumError::Cancelled)
    ));
}

#[test]
fn a_change_during_streaming_never_returns_a_current_checksum() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("large");
    let mut file = File::create(&path).unwrap();
    file.write_all(&vec![7_u8; 256 * 1024]).unwrap();
    let replacement = path.clone();
    let mut replaced = false;

    let result = ChecksumService::compute_with_progress(
        &path,
        ChecksumAlgorithm::Blake3,
        CancellationToken::new(),
        |_| {
            if !replaced {
                replaced = true;
                fs::remove_file(&replacement).unwrap();
                fs::write(&replacement, b"replacement").unwrap();
            }
        },
    );

    assert!(matches!(result, Err(ChecksumError::Changed)));
}

#[test]
fn properties_model_keeps_pages_read_only_and_detects_replaced_targets() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("selected");
    fs::write(&path, b"first").unwrap();
    let snapshot = PropertySnapshot::load(std::slice::from_ref(&path)).unwrap();
    let mut model = PropertiesDialogModel::new(snapshot);

    assert_eq!(model.state(), PropertiesState::Ready);
    assert_eq!(model.page(), PropertiesPage::General);
    assert!(model.pages().contains(&PropertiesPage::Permissions));
    assert!(model.pages().contains(&PropertiesPage::Checksums));
    assert!(!model.apply_visible());
    assert!(model.permissions().edit_disabled_reason().is_some());
    model.select_page(PropertiesPage::Permissions).unwrap();
    assert_eq!(model.page(), PropertiesPage::Permissions);

    fs::remove_file(&path).unwrap();
    fs::write(&path, b"second").unwrap();
    assert_eq!(model.refresh().unwrap(), PropertyRefresh::Replaced);
    assert_eq!(model.state(), PropertiesState::Replaced);
}

#[test]
fn open_with_separates_one_time_launch_from_default_changes() {
    let mut model = OpenWithModel::new(
        "text/plain",
        vec![
            ApplicationChoice::new("writer.desktop", "Writer", true),
            ApplicationChoice::new("image.desktop", "Image Viewer", false),
        ],
    );
    assert_eq!(model.compatible_applications().len(), 1);
    model.select("writer.desktop").unwrap();

    let once = model.plan(OpenWithIntent::OpenOnce).unwrap();
    assert_eq!(once.desktop_id(), "writer.desktop");
    assert!(!once.set_as_default());
    let default = model.plan(OpenWithIntent::SetAsDefault).unwrap();
    assert!(default.set_as_default());
}
