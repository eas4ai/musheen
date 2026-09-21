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

    let reason = entry.unavailable_reason().unwrap();
    assert_eq!(reason, "volume-mount-before-opening");
    let arabic = Catalog::load(Locale::Ar).unwrap().localize_reason(reason);
    assert!(!arabic.contains("mount the volume"));
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
