use musheen_core::StorePath;
use musheen_local::LocalStore;
use musheen_ops::{
    AclChange, AclEntry, AclQualifier, BatchRenameJournal, BatchRenamePlan, BatchRenameStep,
    CreateKind, CreateRequest, DeleteTarget, HardLinkRequest, MetadataChange, MetadataPlan,
    MetadataScope, MutationError, MutationProvider, PermanentDeleteRequest, RenameMapping,
    RenameRequest, SymbolicLinkRequest, execute_create, execute_delete, execute_hard_link,
    execute_permanent_delete, execute_rename, execute_restore, execute_symbolic_link,
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
        execute_restore(&mut store, &outcome.trashed()[0]).unwrap();
        assert_eq!(
            fs::read(path.as_unix_path().unwrap()).unwrap(),
            b"restore me"
        );
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
