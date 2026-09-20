use musheen_core::{
    CancellationToken, CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState,
    CommandTargetRef, ItemId, ItemKind, ProviderId, ResourceLimits, StorePath,
};
use musheen_desktop::{
    AggregateValue, ChecksumAlgorithm, ChecksumError, ChecksumService, PropertyRefresh,
    PropertySnapshot, RecursiveSize, XattrState,
};
use musheen_ui::{
    ApplicationChoice, LocalOperationQueue, OpenWithIntent, OpenWithModel, PropertiesDialogModel,
    PropertiesPage, PropertiesState, ProviderPropertiesDialogModel,
};
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
#[test]
fn snapshots_cover_file_folder_symlink_xattrs_and_mixed_values() {
    use std::os::unix::fs::symlink;

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
fn properties_model_only_offers_apply_for_dirty_valid_reviewed_edits() {
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
    assert!(model.permissions().edit_disabled_reason().is_none());
    model.select_page(PropertiesPage::Permissions).unwrap();
    assert_eq!(model.page(), PropertiesPage::Permissions);
    model.permissions_mut().set_file_mode_text("not-octal");
    assert_eq!(
        model.permissions().edit_disabled_reason(),
        Some("mode must be an octal number")
    );
    assert!(!model.apply_visible());
    model.permissions_mut().set_file_mode_text("0640");
    assert!(model.permissions().edit_disabled_reason().is_none());
    model.permissions_mut().set_recursive(false);
    assert!(!model.apply_visible());
    model.permissions_mut().review_recursive_scope();
    assert!(model.apply_visible());
    let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
    let jobs = model.submit_permissions(&mut queue).unwrap();
    assert_eq!(jobs.len(), 1);
    for operation in queue.start_ready().unwrap() {
        let id = operation.id();
        let result = operation.execute();
        queue.finish(id, result).unwrap();
    }
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
        0o640
    );

    fs::remove_file(&path).unwrap();
    fs::write(&path, b"second").unwrap();
    assert_eq!(model.refresh().unwrap(), PropertyRefresh::Replaced);
    assert_eq!(model.state(), PropertiesState::Replaced);
}

#[test]
fn properties_tags_page_presents_and_edits_the_shared_tag_model() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("tagged");
    fs::write(&path, b"contents").unwrap();
    let snapshot = PropertySnapshot::load(std::slice::from_ref(&path)).unwrap();
    let mut model = PropertiesDialogModel::new(snapshot);
    model.set_tags(["blue", "reviewed"]);

    model.select_page(PropertiesPage::Tags).unwrap();
    assert_eq!(model.tags().collect::<Vec<_>>(), ["blue", "reviewed"]);
    assert!(model.assign_tag("work").unwrap());
    assert!(model.remove_tag("blue"));
    assert_eq!(model.tags().collect::<Vec<_>>(), ["reviewed", "work"]);
    assert!(model.tags_dirty());
}

#[test]
fn provider_opaque_properties_keep_identity_tags_and_only_applicable_pages() {
    let provider = ProviderId::new("remote").unwrap();
    let target = CommandTargetRef::new(
        ItemId::new(provider.clone(), b"stable-object".to_vec()).unwrap(),
        StorePath::from_provider_key(provider, b"share/object".to_vec()).unwrap(),
    )
    .unwrap();
    let capabilities = CapabilityMatrix::new(|kind| {
        if kind == CapabilityKind::Tags {
            CapabilityState::Supported
        } else {
            CapabilityState::Unsupported(CapabilityReason::new("remote metadata only").unwrap())
        }
    });

    let mut model = ProviderPropertiesDialogModel::new(vec![(target.clone(), capabilities)]);
    model.set_tags(["remote"]);

    assert_eq!(model.targets(), &[target]);
    assert_eq!(
        model.pages(),
        &[PropertiesPage::General, PropertiesPage::Tags]
    );
    assert_eq!(model.tags().collect::<Vec<_>>(), ["remote"]);
    assert!(model.assign_tag("shared").unwrap());
    assert_eq!(model.tags().collect::<Vec<_>>(), ["remote", "shared"]);
}

#[cfg(unix)]
#[test]
fn properties_model_never_discards_dirty_permissions_during_refresh() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("selected");
    fs::write(&path, b"contents").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let snapshot = PropertySnapshot::load(std::slice::from_ref(&path)).unwrap();
    let mut model = PropertiesDialogModel::new(snapshot);

    model.permissions_mut().set_file_mode_text("0600");
    assert!(model.permissions().is_dirty());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

    assert_eq!(model.refresh().unwrap(), PropertyRefresh::MetadataChanged);
    assert_eq!(model.state(), PropertiesState::Replaced);
    assert!(model.permissions().is_dirty());
    assert!(!model.apply_visible());
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
