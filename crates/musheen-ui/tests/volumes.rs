#![cfg(unix)]

use musheen_desktop::{
    BackendSnapshot, Capacity, DeviceDescriptor, MountProvider, MountRecord, NoOperationUsage,
    UDisksBackend, UDisksError, VolumeAction, VolumeError, VolumeId, VolumeService,
};
use musheen_ui::dialogs::VolumePropertiesModel;
use musheen_ui::sidebar::{PinStore, SidebarModel, SidebarSectionKind};
use musheen_ui::{Catalog, Locale};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct Backend(Mutex<BackendSnapshot>);

impl UDisksBackend for Backend {
    fn snapshot(&self) -> Result<BackendSnapshot, UDisksError> {
        Ok(self.0.lock().unwrap().clone())
    }

    fn perform(
        &self,
        _volume: &DeviceDescriptor,
        _action: VolumeAction,
        _unlock_secret: Option<&str>,
    ) -> Result<(), UDisksError> {
        Ok(())
    }
}

#[derive(Default)]
struct Mounts {
    records: Mutex<Vec<MountRecord>>,
    capacities: Mutex<BTreeMap<PathBuf, Capacity>>,
}

impl MountProvider for Mounts {
    fn snapshot(&self) -> Result<Vec<MountRecord>, VolumeError> {
        Ok(self.records.lock().unwrap().clone())
    }

    fn capacity(&self, path: &Path) -> Result<Capacity, VolumeError> {
        self.capacities
            .lock()
            .unwrap()
            .get(path)
            .copied()
            .ok_or_else(|| VolumeError::Capacity {
                path: path.to_path_buf(),
                reason: "fixture".into(),
            })
    }
}

fn descriptor() -> DeviceDescriptor {
    DeviceDescriptor::new(
        VolumeId::new("photos").unwrap(),
        "/org/freedesktop/UDisks2/block_devices/sdb1",
    )
    .with_label("Photos")
    .with_device("/dev/sdb1")
    .with_size_bytes(1_000)
    .with_mount_points([PathBuf::from("/media/photos")])
    .with_capabilities(false, true, true, false, true)
}

fn build_service(mounts: Arc<Mounts>) -> VolumeService {
    VolumeService::new(
        Arc::new(Backend(Mutex::new(BackendSnapshot::new(
            "owner",
            [descriptor()],
        )))),
        mounts,
        Arc::new(NoOperationUsage),
    )
}

struct NamedMount<'a> {
    id: &'a str,
    label: &'a str,
    device: &'a str,
    mount: &'a str,
}

fn sidebar_with_named_mounts(devices: &[NamedMount<'_>]) -> SidebarModel {
    let mounts = Arc::new(Mounts::default());
    *mounts.records.lock().unwrap() = devices
        .iter()
        .map(|item| MountRecord::new(item.device, item.mount, "ext4", false))
        .collect();
    let descriptors = devices.iter().map(|item| {
        DeviceDescriptor::new(
            VolumeId::new(item.id).unwrap(),
            format!("/org/freedesktop/UDisks2/block_devices/{}", item.id),
        )
        .with_label(item.label)
        .with_device(item.device)
        .with_mount_points([PathBuf::from(item.mount)])
    });
    let mut service = VolumeService::new(
        Arc::new(Backend(Mutex::new(BackendSnapshot::new(
            "owner",
            descriptors,
        )))),
        mounts,
        Arc::new(NoOperationUsage),
    );
    service.refresh().unwrap();
    let mut sidebar = SidebarModel::new(PinStore::default());
    sidebar.sync_volumes(service.model());
    sidebar
}

#[test]
fn sidebar_projection_uses_live_mount_location_capacity_and_read_only_state() {
    let mounts = Arc::new(Mounts::default());
    *mounts.records.lock().unwrap() = vec![MountRecord::new(
        "/dev/sdb1",
        "/media/photos",
        "ext4",
        false,
    )];
    mounts
        .capacities
        .lock()
        .unwrap()
        .insert("/media/photos".into(), Capacity::new(1_000, 400));
    let mut service = build_service(mounts.clone());
    service.refresh().unwrap();
    let mut sidebar = SidebarModel::new(PinStore::default());

    sidebar.sync_volumes(service.model());
    let storage = sidebar
        .sections()
        .into_iter()
        .find(|section| section.kind() == SidebarSectionKind::Mounts)
        .unwrap();
    let entry = &storage.items()[0];
    assert_eq!(entry.volume_id().unwrap().as_str(), "photos");
    assert_eq!(
        entry.navigation_location().as_unix_path(),
        Some(Path::new("/media/photos"))
    );
    assert_eq!(entry.volume_capacity(), Some(Capacity::new(1_000, 400)));
    assert!(!entry.volume_read_only());

    *mounts.records.lock().unwrap() =
        vec![MountRecord::new("/dev/sdb1", "/media/photos", "ext4", true)];
    mounts
        .capacities
        .lock()
        .unwrap()
        .insert("/media/photos".into(), Capacity::new(1_000, 100));
    service.refresh().unwrap();
    sidebar.sync_volumes(service.model());
    let storage = sidebar
        .sections()
        .into_iter()
        .find(|section| section.kind() == SidebarSectionKind::Mounts)
        .unwrap();
    let entry = &storage.items()[0];
    assert_eq!(entry.volume_capacity(), Some(Capacity::new(1_000, 100)));
    assert!(entry.volume_read_only());
}

#[test]
fn rapid_volume_refreshes_reprobe_capacity() {
    let mounts = Arc::new(Mounts::default());
    *mounts.records.lock().unwrap() = vec![MountRecord::new(
        "/dev/sdb1",
        "/media/photos",
        "ext4",
        false,
    )];
    let mut service = build_service(mounts.clone());

    for available in 1..=32 {
        mounts
            .capacities
            .lock()
            .unwrap()
            .insert("/media/photos".into(), Capacity::new(1_000, available));
        service.refresh().unwrap();
        let volume = service.model().volumes().into_iter().next().unwrap();
        assert_eq!(volume.capacity(), Some(Capacity::new(1_000, available)));
    }
}

#[test]
fn unmounted_sidebar_refusal_is_a_localizable_reason_key() {
    let mounts = Arc::new(Mounts::default());
    let mut service = build_service(mounts);
    service.refresh().unwrap();
    let mut sidebar = SidebarModel::new(PinStore::default());
    sidebar.sync_volumes(service.model());
    let storage = sidebar
        .sections()
        .into_iter()
        .find(|section| section.kind() == SidebarSectionKind::Mounts)
        .unwrap();
    let entry = &storage.items()[0];

    assert_eq!(entry.volume_capacity(), None);
    let reason = entry.unavailable_reason().unwrap();
    assert_eq!(reason, "volume-mount-before-opening");
    let arabic = Catalog::load(Locale::Ar).unwrap().localize_reason(reason);
    assert!(!arabic.contains("mount the volume"));
}

#[test]
fn sidebar_hides_devices_marked_for_non_file_manager_use() {
    let mounts = Arc::new(Mounts::default());
    let hidden = DeviceDescriptor::new(
        VolumeId::new("loop0").unwrap(),
        "/org/freedesktop/UDisks2/block_devices/loop0",
    )
    .with_label("loop0")
    .with_device("/dev/loop0")
    .with_sidebar_visible(false);
    let mut service = VolumeService::new(
        Arc::new(Backend(Mutex::new(BackendSnapshot::new("owner", [hidden])))),
        mounts,
        Arc::new(NoOperationUsage),
    );
    service.refresh().unwrap();
    let mut sidebar = SidebarModel::new(PinStore::default());

    sidebar.sync_volumes(service.model());

    assert!(
        sidebar
            .sections()
            .into_iter()
            .all(|section| section.kind() != SidebarSectionKind::Mounts)
    );
}

#[test]
fn sidebar_shows_internal_data_volume_mounted_in_user_storage() {
    let mounts = Arc::new(Mounts::default());
    let data_mount = PathBuf::from("/home/musheen-user/workspace2");
    *mounts.records.lock().unwrap() = vec![MountRecord::new(
        "/dev/nvme0n1p5",
        data_mount.clone(),
        "ext4",
        false,
    )];
    let internal_data = DeviceDescriptor::new(
        VolumeId::new("workspace2").unwrap(),
        "/org/freedesktop/UDisks2/block_devices/nvme0n1p5",
    )
    .with_label("workspace2")
    .with_device("/dev/nvme0n1p5")
    .with_sidebar_visible(false);
    let mut service = VolumeService::new(
        Arc::new(Backend(Mutex::new(BackendSnapshot::new(
            "owner",
            [internal_data],
        )))),
        mounts,
        Arc::new(NoOperationUsage),
    );
    service.refresh().unwrap();
    let mut sidebar = SidebarModel::new(PinStore::default());

    sidebar.sync_volumes(service.model());

    let storage = sidebar
        .sections()
        .into_iter()
        .find(|section| section.kind() == SidebarSectionKind::Mounts)
        .expect("a mounted internal data volume remains visible");
    assert_eq!(storage.items().len(), 1);
    assert_eq!(
        storage.items()[0].navigation_location().as_unix_path(),
        Some(data_mount.as_path())
    );
}

#[test]
fn sidebar_distinguishes_volumes_with_the_same_device_label() {
    let sidebar = sidebar_with_named_mounts(&[
        NamedMount {
            id: "projects",
            label: "data",
            device: "/dev/nvme4n1p2",
            mount: "/home/shawn/projects",
        },
        NamedMount {
            id: "workspace2",
            label: "data",
            device: "/dev/nvme3n1p1",
            mount: "/home/shawn/workspace2",
        },
        NamedMount {
            id: "data",
            label: "data",
            device: "/dev/nvme2n1p1",
            mount: "/run/media/shawn/data",
        },
        NamedMount {
            id: "photos",
            label: "projects",
            device: "/dev/sdb1",
            mount: "/media/photos",
        },
    ]);
    let storage = sidebar
        .sections()
        .into_iter()
        .find(|section| section.kind() == SidebarSectionKind::Mounts)
        .unwrap();
    let labels_by_mount = storage
        .items()
        .iter()
        .map(|entry| {
            (
                entry.navigation_location().as_unix_path().unwrap(),
                entry.label(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        labels_by_mount[Path::new("/home/shawn/projects")],
        "/home/shawn/projects"
    );
    assert_eq!(
        labels_by_mount[Path::new("/home/shawn/workspace2")],
        "workspace2"
    );
    assert_eq!(labels_by_mount[Path::new("/run/media/shawn/data")], "data");
    assert_eq!(labels_by_mount[Path::new("/media/photos")], "projects");
}

#[test]
fn sidebar_uses_full_mount_paths_when_duplicate_labels_share_a_folder_name() {
    let sidebar = sidebar_with_named_mounts(&[
        NamedMount {
            id: "first",
            label: "data",
            device: "/dev/sdb1",
            mount: "/media/alice/data",
        },
        NamedMount {
            id: "second",
            label: "data",
            device: "/dev/sdc1",
            mount: "/media/bob/data",
        },
    ]);
    let storage = sidebar
        .sections()
        .into_iter()
        .find(|section| section.kind() == SidebarSectionKind::Mounts)
        .unwrap();
    let labels = storage
        .items()
        .iter()
        .map(|entry| entry.label())
        .collect::<Vec<_>>();
    assert_eq!(labels, ["/media/alice/data", "/media/bob/data"]);
}

#[test]
fn sidebar_mount_fallback_keeps_user_storage_and_hides_system_filesystems() {
    let mounts = Arc::new(Mounts::default());
    *mounts.records.lock().unwrap() = vec![
        MountRecord::new("bpf", "/sys/fs/bpf", "bpf", false),
        MountRecord::new("cgroup2", "/sys/fs/cgroup", "cgroup2", false),
        MountRecord::new(
            "overlay",
            "/var/lib/docker/overlay2/example",
            "overlay",
            false,
        ),
        MountRecord::new(
            "gvfsd-fuse",
            "/run/user/1000/gvfs",
            "fuse.gvfsd-fuse",
            false,
        ),
        MountRecord::new("/dev/sdc1", "/mnt/archive", "ext4", false),
        MountRecord::new(
            "/dev/nvme0n1p5",
            "/home/musheen-user/projects",
            "ext4",
            false,
        ),
    ];
    let mut service = VolumeService::new(
        Arc::new(Backend(Mutex::new(BackendSnapshot::new(
            "owner",
            Vec::<DeviceDescriptor>::new(),
        )))),
        mounts,
        Arc::new(NoOperationUsage),
    );
    service.refresh().unwrap();
    let mut sidebar = SidebarModel::new(PinStore::default());

    sidebar.sync_volumes(service.model());

    let storage = sidebar
        .sections()
        .into_iter()
        .find(|section| section.kind() == SidebarSectionKind::Mounts)
        .expect("user storage mounts remain visible without UDisks");
    let paths = storage
        .items()
        .iter()
        .filter_map(|entry| entry.navigation_location().as_unix_path())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        [
            Path::new("/home/musheen-user/projects"),
            Path::new("/mnt/archive"),
        ]
    );
}

#[test]
fn sidebar_hides_loop_mounts_when_udisks_is_unavailable() {
    let mounts = Arc::new(Mounts::default());
    *mounts.records.lock().unwrap() = vec![MountRecord::new(
        "/dev/loop0",
        "/snap/example",
        "squashfs",
        true,
    )];
    let mut service = VolumeService::new(
        Arc::new(Backend(Mutex::new(BackendSnapshot::new(
            "owner",
            Vec::<DeviceDescriptor>::new(),
        )))),
        mounts,
        Arc::new(NoOperationUsage),
    );
    service.refresh().unwrap();
    let mut sidebar = SidebarModel::new(PinStore::default());

    sidebar.sync_volumes(service.model());

    assert!(
        sidebar
            .sections()
            .into_iter()
            .all(|section| section.kind() != SidebarSectionKind::Mounts)
    );
}

#[test]
fn volume_properties_model_updates_without_reopening_the_dialog() {
    let mounts = Arc::new(Mounts::default());
    *mounts.records.lock().unwrap() = vec![MountRecord::new(
        "/dev/sdb1",
        "/media/photos",
        "ext4",
        false,
    )];
    mounts
        .capacities
        .lock()
        .unwrap()
        .insert("/media/photos".into(), Capacity::new(1_000, 400));
    let mut service = build_service(mounts.clone());
    service.refresh().unwrap();
    let mut properties = VolumePropertiesModel::new(
        service
            .model()
            .get(&VolumeId::new("photos").unwrap())
            .unwrap(),
    );

    assert_eq!(properties.available_bytes(), Some(400));
    assert!(!properties.is_read_only());

    *mounts.records.lock().unwrap() =
        vec![MountRecord::new("/dev/sdb1", "/media/photos", "ext4", true)];
    mounts
        .capacities
        .lock()
        .unwrap()
        .insert("/media/photos".into(), Capacity::new(1_000, 125));
    service.refresh().unwrap();
    assert!(
        properties.update(
            service
                .model()
                .get(&VolumeId::new("photos").unwrap())
                .unwrap()
        )
    );
    assert_eq!(properties.available_bytes(), Some(125));
    assert!(properties.is_read_only());
}
