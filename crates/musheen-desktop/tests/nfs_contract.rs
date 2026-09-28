use futures_lite::future::block_on;
use musheen_core::{
    CancellationToken, CapabilityKind, CapabilityState, PageRequest, ResourceLimits, Store,
    StorePath,
};
use musheen_desktop::remote::{MountedNfsStore, NfsMount, NfsMountSource};
use std::path::PathBuf;

#[derive(Clone)]
struct StaticMounts(Vec<NfsMount>);

impl NfsMountSource for StaticMounts {
    fn mounts(&self) -> Result<Vec<NfsMount>, musheen_core::StoreError> {
        Ok(self.0.clone())
    }
}

#[test]
fn nfs_accepts_only_paths_below_a_kernel_nfs_mount() {
    let directory = tempfile::tempdir().expect("temporary mount point");
    let nfs =
        NfsMount::new("server:/export", directory.path(), "nfs4", false).expect("valid NFS mount");
    let store = MountedNfsStore::new(directory.path(), StaticMounts(vec![nfs]))
        .expect("kernel-mounted NFS store");

    assert_eq!(store.mount_point(), directory.path());
    assert_eq!(store.mount_source().to_string_lossy(), "server:/export");
    let page = block_on(store.read_directory(
        &StorePath::from_unix_path(directory.path()),
        PageRequest::first(&ResourceLimits::default()),
        CancellationToken::new(),
    ))
    .expect("local-provider directory read");
    assert!(page.items().is_empty());

    let local =
        NfsMount::new("/dev/sda", directory.path(), "ext4", false).expect("valid mount record");
    assert!(MountedNfsStore::new(directory.path(), StaticMounts(vec![local])).is_err());
    assert!(MountedNfsStore::new(directory.path(), StaticMounts(vec![])).is_err());
    assert!(
        store
            .resolve_item(&StorePath::from_unix_path("/etc/passwd"))
            .is_err()
    );
}

#[test]
fn nfs_rejects_parent_components_that_escape_the_mount() {
    let root = tempfile::tempdir().expect("temporary mount root");
    let nfs = NfsMount::new("server:/export", root.path(), "nfs", false).expect("NFS mount");
    let escaped = root.path().join("..").join("outside");
    assert!(MountedNfsStore::new(escaped, StaticMounts(vec![nfs])).is_err());
}

#[test]
fn longest_matching_mount_wins_and_read_only_state_restricts_mutations() {
    let root = tempfile::tempdir().expect("temporary mount root");
    let nested = root.path().join("nested");
    std::fs::create_dir(&nested).expect("nested mount point");
    let mounts = StaticMounts(vec![
        NfsMount::new("server:/root", root.path(), "nfs", false).expect("root mount"),
        NfsMount::new("server:/nested", &nested, "nfs4", true).expect("nested mount"),
    ]);
    let store = MountedNfsStore::new(&nested, mounts).expect("nested NFS store");

    assert_eq!(store.mount_source(), PathBuf::from("server:/nested"));
    assert!(matches!(
        store
            .capabilities(&StorePath::from_unix_path(&nested))
            .get(CapabilityKind::AtomicRename),
        CapabilityState::Unsupported(_)
    ));
    assert!(matches!(
        store
            .capabilities(&StorePath::from_unix_path(&nested))
            .get(CapabilityKind::CaseSensitivity),
        CapabilityState::Supported
    ));
}
