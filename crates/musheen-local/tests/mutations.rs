use musheen_core::StorePath;
use musheen_local::LocalStore;
use musheen_ops::{
    AclChange, AclEntry, AclQualifier, BatchRenameJournal, BatchRenamePlan, BatchRenameStep,
    ConflictChoice, ConflictDecision, ConflictDecisionJournal, ConflictItemKind, ConflictPolicies,
    ConflictRecord, CreateKind, CreateRequest, DeleteTarget, HardLinkRequest, MetadataChange,
    MetadataPlan, MetadataScope, MutationError, MutationProvider, OperationKind,
    PermanentDeleteRequest, RenameMapping, RenameRequest, SymbolicLinkRequest, execute_create,
    execute_delete, execute_hard_link, execute_permanent_delete, execute_rename, execute_restore,
    execute_symbolic_link,
};
use posix_acl::{ACL_READ, PosixACL, Qualifier};
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::process::Command;
use tempfile::tempdir;

#[derive(Default)]
struct TestJournal {
    planned: bool,
    completed: usize,
}

#[derive(Default)]
struct TestConflictJournal;

impl ConflictDecisionJournal for TestConflictJournal {
    fn persist_decision(&mut self, _decision: &ConflictDecision) -> Result<(), MutationError> {
        Ok(())
    }
}

impl BatchRenameJournal for TestJournal {
    fn persist_plan(&mut self, steps: &[BatchRenameStep]) -> Result<(), MutationError> {
        self.planned = !steps.is_empty();
        Ok(())
    }

    fn persist_completed_step(&mut self, _index: usize) -> Result<(), MutationError> {
        self.completed += 1;
        Ok(())
    }
}

#[test]
fn local_trash_round_trip() {
    if std::env::var_os("MUSHEEN_TRASH_HELPER").is_some() {
        let root = std::path::PathBuf::from(std::env::var_os("MUSHEEN_TRASH_ROOT").unwrap());
        let path = root.join("trashed-file");
        fs::write(&path, b"restore me").unwrap();
        let path = StorePath::from_unix_path(path.into_os_string());
        let mut store = LocalStore::new();
        let identity = MutationProvider::identity(&mut store, &path)
            .unwrap()
            .unwrap();
        let outcome = execute_delete(
            &mut store,
            vec![DeleteTarget::new(path.clone(), identity.to_vec())],
        )
        .unwrap();
        assert_eq!(outcome.trashed().len(), 1);
        assert!(!path.as_unix_path().unwrap().exists());
        let listed = store.list_trash().unwrap();
        let listed_item = listed
            .iter()
            .find(|item| item.receipt() == &outcome.trashed()[0])
            .expect("trashed item is listed with its receipt");
        assert!(listed_item.deleted_at_unix_seconds() > 0);
        fs::write(path.as_unix_path().unwrap(), b"new occupant").unwrap();
        let destination_identity = MutationProvider::identity(&mut store, &path)
            .unwrap()
            .unwrap();
        let conflict = ConflictRecord::new(
            OperationKind::Restore,
            StorePath::from_provider_key(
                musheen_core::ProviderId::new("local.trash").unwrap(),
                outcome.trashed()[0].provider_reference().to_vec(),
            )
            .unwrap(),
            outcome.trashed()[0].provider_reference().to_vec(),
            ConflictItemKind::File,
            path.clone(),
            destination_identity.to_vec(),
            ConflictItemKind::File,
        )
        .unwrap();
        let decision = ConflictPolicies::default()
            .decide(
                &conflict,
                ConflictChoice::KeepBoth,
                musheen_ops::ApplyScope::ThisConflict,
                &mut TestConflictJournal,
            )
            .unwrap();
        store
            .resolve_restore_conflict(&outcome.trashed()[0], &decision)
            .unwrap();
        assert_eq!(
            fs::read(path.as_unix_path().unwrap()).unwrap(),
            b"restore me"
        );
        assert!(
            fs::read_dir(root.as_path())
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry.path() != path.as_unix_path().unwrap()
                        && fs::read(entry.path()).ok().as_deref() == Some(b"new occupant")
                })
        );
        let identity = MutationProvider::identity(&mut store, &path)
            .unwrap()
            .unwrap();
        let outcome = execute_delete(
            &mut store,
            vec![DeleteTarget::new(path.clone(), identity.to_vec())],
        )
        .unwrap();
        store.purge_trash(outcome.trashed()).unwrap();
        assert!(
            store
                .list_trash()
                .unwrap()
                .iter()
                .all(|item| item.receipt() != &outcome.trashed()[0])
        );

        let link_target = root.join("link-target");
        let link_path = root.join("trashed-link");
        fs::write(&link_target, b"target stays").unwrap();
        symlink(&link_target, &link_path).unwrap();
        let link_store_path = StorePath::from_unix_path(link_path.into_os_string());
        let link_identity = MutationProvider::identity(&mut store, &link_store_path)
            .unwrap()
            .unwrap();
        let link_outcome = execute_delete(
            &mut store,
            vec![DeleteTarget::new(link_store_path, link_identity.to_vec())],
        )
        .unwrap();
        let link_receipt = &link_outcome.trashed()[0];
        let listed_link = store
            .list_trash()
            .unwrap()
            .into_iter()
            .find(|item| item.receipt() == link_receipt)
            .expect("the trashed symbolic link remains listable");
        assert_eq!(listed_link.kind(), ConflictItemKind::SymbolicLink);
        store
            .purge_trash(std::slice::from_ref(link_receipt))
            .unwrap();
        assert!(link_target.exists());
        return;
    }

    let root = tempdir().unwrap();
    let xdg_data = root.path().join("xdg-data");
    fs::create_dir(&xdg_data).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("local_trash_round_trip")
        .arg("--nocapture")
        .env("MUSHEEN_TRASH_HELPER", "1")
        .env("MUSHEEN_TRASH_ROOT", root.path())
        .env("XDG_DATA_HOME", xdg_data)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn local_trash_directory_merge_preserves_both_trees() {
    if std::env::var_os("MUSHEEN_TRASH_MERGE_HELPER").is_some() {
        let root = std::path::PathBuf::from(std::env::var_os("MUSHEEN_TRASH_ROOT").unwrap());
        let path = root.join("trashed-directory");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("from-trash"), b"restore me").unwrap();
        let store_path = StorePath::from_unix_path(path.clone().into_os_string());
        let mut store = LocalStore::new();
        let identity = MutationProvider::identity(&mut store, &store_path)
            .unwrap()
            .unwrap();
        let outcome = execute_delete(
            &mut store,
            vec![DeleteTarget::new(store_path.clone(), identity.to_vec())],
        )
        .unwrap();

        fs::create_dir(&path).unwrap();
        fs::write(path.join("already-here"), b"keep me").unwrap();
        let destination_identity = MutationProvider::identity(&mut store, &store_path)
            .unwrap()
            .unwrap();
        let receipt = &outcome.trashed()[0];
        let conflict = ConflictRecord::new(
            OperationKind::Restore,
            StorePath::from_provider_key(
                musheen_core::ProviderId::new("local.trash").unwrap(),
                receipt.provider_reference().to_vec(),
            )
            .unwrap(),
            receipt.provider_reference().to_vec(),
            ConflictItemKind::Directory,
            store_path,
            destination_identity.to_vec(),
            ConflictItemKind::Directory,
        )
        .unwrap();
        let decision = ConflictPolicies::default()
            .decide(
                &conflict,
                ConflictChoice::MergeDirectory,
                musheen_ops::ApplyScope::ThisConflict,
                &mut TestConflictJournal,
            )
            .unwrap();

        store.resolve_restore_conflict(receipt, &decision).unwrap();

        assert_eq!(fs::read(path.join("from-trash")).unwrap(), b"restore me");
        assert_eq!(fs::read(path.join("already-here")).unwrap(), b"keep me");
        assert!(
            store
                .list_trash()
                .unwrap()
                .iter()
                .all(|item| item.receipt() != receipt)
        );

        let colliding_path = root.join("colliding-directory");
        fs::create_dir(&colliding_path).unwrap();
        fs::write(colliding_path.join("same-name"), b"from trash").unwrap();
        let colliding_store_path =
            StorePath::from_unix_path(colliding_path.clone().into_os_string());
        let identity = MutationProvider::identity(&mut store, &colliding_store_path)
            .unwrap()
            .unwrap();
        let colliding_outcome = execute_delete(
            &mut store,
            vec![DeleteTarget::new(
                colliding_store_path.clone(),
                identity.to_vec(),
            )],
        )
        .unwrap();
        fs::create_dir(&colliding_path).unwrap();
        fs::write(colliding_path.join("same-name"), b"existing").unwrap();
        let destination_identity = MutationProvider::identity(&mut store, &colliding_store_path)
            .unwrap()
            .unwrap();
        let colliding_receipt = &colliding_outcome.trashed()[0];
        let conflict = ConflictRecord::new(
            OperationKind::Restore,
            StorePath::from_provider_key(
                musheen_core::ProviderId::new("local.trash").unwrap(),
                colliding_receipt.provider_reference().to_vec(),
            )
            .unwrap(),
            colliding_receipt.provider_reference().to_vec(),
            ConflictItemKind::Directory,
            colliding_store_path,
            destination_identity.to_vec(),
            ConflictItemKind::Directory,
        )
        .unwrap();
        let decision = ConflictPolicies::default()
            .decide(
                &conflict,
                ConflictChoice::MergeDirectory,
                musheen_ops::ApplyScope::ThisConflict,
                &mut TestConflictJournal,
            )
            .unwrap();

        assert_eq!(
            store.resolve_restore_conflict(colliding_receipt, &decision),
            Err(MutationError::Conflict)
        );
        assert_eq!(
            fs::read(colliding_path.join("same-name")).unwrap(),
            b"existing"
        );
        assert!(
            store
                .list_trash()
                .unwrap()
                .iter()
                .any(|item| item.receipt() == colliding_receipt)
        );
        return;
    }

    let root = tempdir().unwrap();
    let xdg_data = root.path().join("xdg-data");
    fs::create_dir(&xdg_data).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("local_trash_directory_merge_preserves_both_trees")
        .arg("--nocapture")
        .env("MUSHEEN_TRASH_MERGE_HELPER", "1")
        .env("MUSHEEN_TRASH_ROOT", root.path())
        .env("XDG_DATA_HOME", xdg_data)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn local_create_preserves_non_utf8_names_and_refuses_existing_entries() {
    let directory = tempdir().unwrap();
    let parent = StorePath::from_unix_path(directory.path().as_os_str());
    let name = OsString::from_vec(vec![b'n', 0xff]);
    let request = CreateRequest::new(parent.clone(), name.clone(), CreateKind::File);
    let mut store = LocalStore::new();

    let created = execute_create(&mut store, &request).unwrap();
    assert!(created.as_unix_path().unwrap().is_file());
    assert_eq!(
        execute_create(&mut store, &request),
        Err(MutationError::Conflict)
    );
}

#[test]
fn local_permanent_delete_removes_the_selected_tree_without_following_symlinks() {
    let directory = tempdir().unwrap();
    let outside = directory.path().join("outside");
    let selected = directory.path().join("selected");
    fs::write(&outside, b"keep").unwrap();
    fs::create_dir(&selected).unwrap();
    fs::write(selected.join("file"), b"remove").unwrap();
    symlink(&outside, selected.join("link")).unwrap();
    let selected = StorePath::from_unix_path(selected.into_os_string());
    let location = StorePath::from_unix_path(directory.path().as_os_str());
    let mut store = LocalStore::new();
    let identity = MutationProvider::identity(&mut store, &selected)
        .unwrap()
        .unwrap();
    let request = PermanentDeleteRequest::new(
        location.clone(),
        vec![DeleteTarget::new(selected.clone(), identity.to_vec())],
    )
    .unwrap();
    let confirmation = request.challenge().confirm(1, &location, true).unwrap();

    execute_permanent_delete(&mut store, &request, &confirmation).unwrap();

    assert!(!selected.as_unix_path().unwrap().exists());
    assert_eq!(fs::read(outside).unwrap(), b"keep");
}

#[test]
fn local_recursive_metadata_separates_modes_and_does_not_follow_symlinks() {
    let directory = tempdir().unwrap();
    let root_path = directory.path().join("root");
    let file_path = root_path.join("file");
    let link_path = root_path.join("link");
    fs::create_dir(&root_path).unwrap();
    fs::write(&file_path, b"content").unwrap();
    symlink("file", &link_path).unwrap();
    fs::set_permissions(&root_path, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o600)).unwrap();
    let root = StorePath::from_unix_path(root_path.as_os_str());
    let mut store = LocalStore::new();
    let identity = MutationProvider::identity(&mut store, &root)
        .unwrap()
        .unwrap();
    let change = MetadataChange::new()
        .with_file_mode(0o640)
        .with_directory_mode(0o750);
    let plan = MetadataPlan::preflight(
        &mut store,
        root,
        identity.to_vec(),
        MetadataScope::recursive(false, true),
        change,
    )
    .unwrap();

    plan.execute(&mut store).unwrap();

    assert_eq!(
        fs::metadata(root_path).unwrap().permissions().mode() & 0o7777,
        0o750
    );
    assert_eq!(
        fs::metadata(file_path).unwrap().permissions().mode() & 0o7777,
        0o640
    );
    assert_eq!(
        fs::read_link(link_path).unwrap(),
        std::path::Path::new("file")
    );
}

#[test]
fn local_metadata_applies_and_removes_posix_acl_entries() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("file");
    fs::write(&path, b"content").unwrap();
    let target = StorePath::from_unix_path(path.as_os_str());
    let mut store = LocalStore::new();
    let identity = MutationProvider::identity(&mut store, &target)
        .unwrap()
        .unwrap();
    let acl = AclChange::Replace(vec![
        AclEntry::new(AclQualifier::Owner, true, true, false),
        AclEntry::new(AclQualifier::OwningGroup, true, false, false),
        AclEntry::new(AclQualifier::Other, false, false, false),
        AclEntry::new(AclQualifier::User(12_345), true, false, false),
    ]);
    let plan = MetadataPlan::preflight(
        &mut store,
        target.clone(),
        identity.to_vec(),
        MetadataScope::Single,
        MetadataChange::new().with_access_acl(acl),
    )
    .unwrap();
    plan.execute(&mut store).unwrap();
    assert_eq!(
        PosixACL::read_acl(&path)
            .unwrap()
            .get(Qualifier::User(12_345)),
        Some(ACL_READ)
    );

    let identity = MutationProvider::identity(&mut store, &target)
        .unwrap()
        .unwrap();
    let remove = MetadataPlan::preflight(
        &mut store,
        target,
        identity.to_vec(),
        MetadataScope::Single,
        MetadataChange::new().with_access_acl(AclChange::Remove),
    )
    .unwrap();
    remove.execute(&mut store).unwrap();
    assert_eq!(
        PosixACL::read_acl(path)
            .unwrap()
            .get(Qualifier::User(12_345)),
        None
    );
}

#[test]
fn local_links_preserve_targets_and_hard_link_identity() {
    let directory = tempdir().unwrap();
    let parent = StorePath::from_unix_path(directory.path().as_os_str());
    let source = StorePath::from_unix_path(directory.path().join("source").into_os_string());
    fs::write(source.as_unix_path().unwrap(), b"content").unwrap();
    let mut store = LocalStore::new();
    let symbolic = SymbolicLinkRequest::new(
        parent.clone(),
        OsString::from("symbolic"),
        OsString::from("source"),
    );
    let symbolic_path = execute_symbolic_link(&mut store, &symbolic).unwrap();
    assert_eq!(
        fs::read_link(symbolic_path.as_unix_path().unwrap()).unwrap(),
        std::path::Path::new("source")
    );

    let identity = MutationProvider::identity(&mut store, &source)
        .unwrap()
        .unwrap();
    let hard = HardLinkRequest::new(
        source.clone(),
        parent,
        OsString::from("hard"),
        identity.to_vec(),
    );
    let hard_path = execute_hard_link(&mut store, &hard).unwrap();
    assert_eq!(
        fs::metadata(source.as_unix_path().unwrap()).unwrap().ino(),
        fs::metadata(hard_path.as_unix_path().unwrap())
            .unwrap()
            .ino()
    );
}

#[test]
fn local_batch_rename_resolves_cycles_without_replacing_data() {
    let directory = tempdir().unwrap();
    let a = StorePath::from_unix_path(directory.path().join("a").into_os_string());
    let b = StorePath::from_unix_path(directory.path().join("b").into_os_string());
    fs::write(a.as_unix_path().unwrap(), b"a").unwrap();
    fs::write(b.as_unix_path().unwrap(), b"b").unwrap();
    let mut store = LocalStore::new();
    let a_identity = store.identity(&a).unwrap().unwrap();
    let b_identity = store.identity(&b).unwrap().unwrap();
    let plan = BatchRenamePlan::preflight(
        &mut store,
        vec![
            RenameMapping::new(a.clone(), OsString::from("b"), a_identity.to_vec()),
            RenameMapping::new(b.clone(), OsString::from("a"), b_identity.to_vec()),
        ],
    )
    .unwrap();

    let mut journal = TestJournal::default();
    plan.execute(&mut store, &mut journal).unwrap();

    assert_eq!(fs::read(a.as_unix_path().unwrap()).unwrap(), b"b");
    assert_eq!(fs::read(b.as_unix_path().unwrap()).unwrap(), b"a");
    assert!(journal.planned);
    assert_eq!(journal.completed, 3);
}

#[test]
fn local_rename_never_replaces_and_revalidates_identity() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source");
    let occupied = directory.path().join("occupied");
    fs::write(&source, b"source").unwrap();
    fs::write(&occupied, b"occupied").unwrap();
    let source = StorePath::from_unix_path(source.into_os_string());
    let mut store = LocalStore::new();
    let identity = store.identity(&source).unwrap().unwrap();

    let conflict = RenameRequest::new(
        source.clone(),
        OsString::from("occupied"),
        identity.to_vec(),
    );
    assert_eq!(
        execute_rename(&mut store, &conflict),
        Err(MutationError::Conflict)
    );

    fs::remove_file(source.as_unix_path().unwrap()).unwrap();
    fs::write(source.as_unix_path().unwrap(), b"replacement").unwrap();
    let stale = RenameRequest::new(source, OsString::from("renamed"), identity.to_vec());
    assert_eq!(
        execute_rename(&mut store, &stale),
        Err(MutationError::SourceChanged)
    );
    assert_eq!(fs::read(occupied).unwrap(), b"occupied");
}

#[test]
fn local_identity_binds_a_hard_link_to_its_opened_parent() {
    let directory = tempdir().unwrap();
    let first_parent = directory.path().join("first");
    let second_parent = directory.path().join("second");
    fs::create_dir(&first_parent).unwrap();
    fs::create_dir(&second_parent).unwrap();
    let first = first_parent.join("file");
    let second = second_parent.join("file");
    fs::write(&first, b"same inode").unwrap();
    fs::hard_link(&first, &second).unwrap();
    let first = StorePath::from_unix_path(first.into_os_string());
    let second = StorePath::from_unix_path(second.into_os_string());
    let mut store = LocalStore::new();

    assert_ne!(
        MutationProvider::identity(&mut store, &first)
            .unwrap()
            .unwrap(),
        MutationProvider::identity(&mut store, &second)
            .unwrap()
            .unwrap()
    );
}

#[test]
fn parent_symlink_swap_cannot_redirect_a_rename_even_for_the_same_inode() {
    let directory = tempdir().unwrap();
    let first_parent = directory.path().join("first");
    let second_parent = directory.path().join("second");
    let alias_parent = directory.path().join("current");
    fs::create_dir(&first_parent).unwrap();
    fs::create_dir(&second_parent).unwrap();
    let first = first_parent.join("file");
    let second = second_parent.join("file");
    fs::write(&first, b"same inode").unwrap();
    fs::hard_link(&first, &second).unwrap();
    symlink(&first_parent, &alias_parent).unwrap();
    let alias = StorePath::from_unix_path(alias_parent.join("file").into_os_string());
    let mut store = LocalStore::new();
    let identity = MutationProvider::identity(&mut store, &alias)
        .unwrap()
        .unwrap();
    fs::remove_file(&alias_parent).unwrap();
    symlink(&second_parent, &alias_parent).unwrap();

    let request = RenameRequest::new(alias, OsString::from("renamed"), identity.to_vec());
    assert_eq!(
        execute_rename(&mut store, &request),
        Err(MutationError::SourceChanged)
    );
    assert!(first.exists());
    assert!(second.exists());
}

#[test]
fn local_trash_restore_returns_a_link_to_a_directory() {
    if std::env::var_os("MUSHEEN_TRASH_LINK_HELPER").is_some() {
        let root = std::path::PathBuf::from(std::env::var_os("MUSHEEN_TRASH_ROOT").unwrap());
        let target = root.join("target-directory");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("inside"), b"stays").unwrap();
        let link = root.join("trashed-link");
        symlink(&target, &link).unwrap();
        let link_path = StorePath::from_unix_path(link.clone().into_os_string());
        let mut store = LocalStore::new();
        let identity = MutationProvider::identity(&mut store, &link_path)
            .unwrap()
            .unwrap();
        let outcome = execute_delete(
            &mut store,
            vec![DeleteTarget::new(link_path, identity.to_vec())],
        )
        .unwrap();
        assert!(fs::symlink_metadata(&link).is_err());
        let receipt = &outcome.trashed()[0];

        execute_restore(&mut store, receipt).expect("a trashed link to a directory restores");

        let restored = fs::symlink_metadata(&link).expect("the link is back at its original path");
        assert!(
            restored.file_type().is_symlink(),
            "the restored entry is the link, not a directory"
        );
        assert_eq!(fs::read_link(&link).unwrap(), target);
        assert_eq!(fs::read(target.join("inside")).unwrap(), b"stays");
        assert!(
            store
                .list_trash()
                .unwrap()
                .iter()
                .all(|item| item.receipt() != receipt)
        );
        return;
    }

    let root = tempdir().unwrap();
    let xdg_data = root.path().join("xdg-data");
    fs::create_dir(&xdg_data).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("local_trash_restore_returns_a_link_to_a_directory")
        .arg("--nocapture")
        .env("MUSHEEN_TRASH_LINK_HELPER", "1")
        .env("MUSHEEN_TRASH_ROOT", root.path())
        .env("XDG_DATA_HOME", xdg_data)
        .status()
        .unwrap();
    assert!(status.success());
}

/// Trashes `path` through the store and returns its receipt.
fn trash_through_store(store: &mut LocalStore, path: &std::path::Path) -> musheen_ops::TrashReceipt {
    let store_path = StorePath::from_unix_path(path.to_path_buf().into_os_string());
    let identity = MutationProvider::identity(store, &store_path)
        .unwrap()
        .unwrap();
    let outcome = execute_delete(
        store,
        vec![DeleteTarget::new(store_path, identity.to_vec())],
    )
    .unwrap();
    outcome.trashed()[0].clone()
}

/// Runs this test binary again with a private trash: `XDG_DATA_HOME` points
/// into a fresh temporary directory and `MUSHEEN_TRASH_ROOT` at the folder
/// the helper works in.
fn run_trash_helper(test_name: &str, helper_variable: &str) {
    let root = tempdir().unwrap();
    let xdg_data = root.path().join("xdg-data");
    fs::create_dir(&xdg_data).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(helper_variable, "1")
        .env("MUSHEEN_TRASH_ROOT", root.path())
        .env("XDG_DATA_HOME", xdg_data)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn local_trash_listing_shows_an_unreadable_record_as_unrestorable_and_purgeable() {
    if std::env::var_os("MUSHEEN_TRASH_RECORD_HELPER").is_some() {
        let root = std::path::PathBuf::from(std::env::var_os("MUSHEEN_TRASH_ROOT").unwrap());
        let kept_path = root.join("kept");
        fs::write(&kept_path, b"kept").unwrap();
        let mut store = LocalStore::new();
        let kept = trash_through_store(&mut store, &kept_path);
        let trash = std::path::PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap())
            .join("Trash");
        // A record nothing can parse, with its data still in Trash.
        fs::write(trash.join("info").join("garbled.trashinfo"), b"not a trash record\n").unwrap();
        fs::write(trash.join("files").join("garbled"), b"data").unwrap();
        // A record the process may not read, with its data in a folder.
        fs::write(
            trash.join("info").join("sealed.trashinfo"),
            format!(
                "[Trash Info]\nPath={}\nDeletionDate=2026-09-25T08:00:00\n",
                root.join("sealed").display()
            ),
        )
        .unwrap();
        fs::set_permissions(
            trash.join("info").join("sealed.trashinfo"),
            fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        fs::create_dir(trash.join("files").join("sealed")).unwrap();

        let listed = store
            .list_trash()
            .expect("unreadable records never hide the other entries");

        assert!(
            listed
                .iter()
                .any(|item| item.receipt() == &kept && item.restorable()),
            "the readable entry is listed and restorable"
        );
        let garbled = listed
            .iter()
            .find(|item| item.receipt().provider_reference().ends_with(b"garbled.trashinfo"))
            .expect("the garbled record is listed");
        assert!(!garbled.restorable());
        assert_eq!(garbled.kind(), ConflictItemKind::File);
        assert_eq!(
            garbled.receipt().original_path(),
            &StorePath::from_unix_path(trash.join("files").join("garbled").into_os_string()),
            "an entry whose original location is unknown shows where its data is"
        );
        let sealed = listed
            .iter()
            .find(|item| item.receipt().provider_reference().ends_with(b"sealed.trashinfo"))
            .expect("the unreadable record is listed");
        assert!(!sealed.restorable());
        assert_eq!(sealed.kind(), ConflictItemKind::Directory);
        assert!(
            execute_restore(&mut store, garbled.receipt()).is_err(),
            "an entry without a readable record does not restore"
        );
        assert_eq!(fs::read(trash.join("files").join("garbled")).unwrap(), b"data");

        store
            .purge_trash(&[garbled.receipt().clone(), sealed.receipt().clone()])
            .expect("unreadable records can be purged");

        // The listing also shows the trash folders of other mounts, so only
        // the entries of the private trash count.
        let own = store
            .list_trash()
            .unwrap()
            .into_iter()
            .filter(|item| {
                std::path::PathBuf::from(OsString::from_vec(
                    item.receipt().provider_reference().to_vec(),
                ))
                .starts_with(&trash)
            })
            .collect::<Vec<_>>();
        assert_eq!(own.len(), 1, "{own:?}");
        assert_eq!(own[0].receipt(), &kept);
        assert!(!trash.join("files").join("garbled").exists());
        assert!(!trash.join("files").join("sealed").exists());
        assert!(!trash.join("info").join("garbled.trashinfo").exists());
        assert!(!trash.join("info").join("sealed.trashinfo").exists());
        return;
    }

    run_trash_helper(
        "local_trash_listing_shows_an_unreadable_record_as_unrestorable_and_purgeable",
        "MUSHEEN_TRASH_RECORD_HELPER",
    );
}

#[test]
fn local_trash_purge_removes_a_read_only_tree_and_the_other_entries() {
    if std::env::var_os("MUSHEEN_TRASH_LOCKED_HELPER").is_some() {
        let root = std::path::PathBuf::from(std::env::var_os("MUSHEEN_TRASH_ROOT").unwrap());
        let locked = root.join("locked");
        fs::create_dir(&locked).unwrap();
        let inner = locked.join("inner");
        fs::create_dir(&inner).unwrap();
        fs::write(inner.join("file"), b"read-only").unwrap();
        fs::set_permissions(inner.join("file"), fs::Permissions::from_mode(0o444)).unwrap();
        fs::set_permissions(&inner, fs::Permissions::from_mode(0o555)).unwrap();
        let kept_path = root.join("kept");
        fs::write(&kept_path, b"kept").unwrap();
        let mut store = LocalStore::new();
        let locked_receipt = trash_through_store(&mut store, &locked);
        let kept_receipt = trash_through_store(&mut store, &kept_path);

        store
            .purge_trash(&[locked_receipt, kept_receipt])
            .expect("a tree with a read-only folder is purged with the rest");

        let trash = std::path::PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap())
            .join("Trash");
        // The listing also shows the trash folders of other mounts, so only
        // the entries of the private trash count.
        assert!(store.list_trash().unwrap().iter().all(|item| {
            !std::path::PathBuf::from(OsString::from_vec(
                item.receipt().provider_reference().to_vec(),
            ))
            .starts_with(&trash)
        }));
        assert_eq!(
            fs::read_dir(trash.join("files")).unwrap().count(),
            0,
            "no data is left in Trash"
        );
        return;
    }

    run_trash_helper(
        "local_trash_purge_removes_a_read_only_tree_and_the_other_entries",
        "MUSHEEN_TRASH_LOCKED_HELPER",
    );
}

#[test]
fn local_trash_listing_survives_an_orphaned_info_file() {
    if std::env::var_os("MUSHEEN_TRASH_ORPHAN_HELPER").is_some() {
        let root = std::path::PathBuf::from(std::env::var_os("MUSHEEN_TRASH_ROOT").unwrap());
        let path = root.join("kept");
        fs::write(&path, b"kept").unwrap();
        let store_path = StorePath::from_unix_path(path.into_os_string());
        let mut store = LocalStore::new();
        let identity = MutationProvider::identity(&mut store, &store_path)
            .unwrap()
            .unwrap();
        let outcome = execute_delete(
            &mut store,
            vec![DeleteTarget::new(store_path, identity.to_vec())],
        )
        .unwrap();
        let receipt = &outcome.trashed()[0];
        let orphan = std::path::PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap())
            .join("Trash")
            .join("info")
            .join("orphan.trashinfo");
        fs::write(
            &orphan,
            format!(
                "[Trash Info]\nPath={}\nDeletionDate=2026-09-25T08:00:00\n",
                root.join("orphan").display()
            ),
        )
        .unwrap();

        let listed = store
            .list_trash()
            .expect("one info file without a payload never hides the other entries");

        let kept = listed
            .iter()
            .find(|item| item.receipt() == receipt)
            .expect("the entry with its data is listed");
        assert!(kept.restorable());
        let orphan_path = StorePath::from_unix_path(root.join("orphan").into_os_string());
        let orphan = listed
            .iter()
            .find(|item| item.receipt().original_path() == &orphan_path)
            .expect("the entry without its data is listed too");
        assert!(
            !orphan.restorable(),
            "an entry whose data is missing is unrestorable"
        );
        assert_eq!(
            execute_restore(&mut store, orphan.receipt()),
            Err(MutationError::Missing)
        );
        store
            .purge_trash(std::slice::from_ref(orphan.receipt()))
            .expect("an unrestorable entry can be purged");
        let after = store.list_trash().unwrap();
        assert!(after.iter().all(|item| item.receipt() != orphan.receipt()));
        assert!(after.iter().any(|item| item.receipt() == receipt));
        return;
    }

    let root = tempdir().unwrap();
    let xdg_data = root.path().join("xdg-data");
    fs::create_dir(&xdg_data).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("local_trash_listing_survives_an_orphaned_info_file")
        .arg("--nocapture")
        .env("MUSHEEN_TRASH_ORPHAN_HELPER", "1")
        .env("MUSHEEN_TRASH_ROOT", root.path())
        .env("XDG_DATA_HOME", xdg_data)
        .status()
        .unwrap();
    assert!(status.success());
}
