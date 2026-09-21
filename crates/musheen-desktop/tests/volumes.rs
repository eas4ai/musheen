#![cfg(unix)]

use musheen_desktop::{
    BackendSnapshot, Capacity, DeviceDescriptor, MountOperation, MountProvider, MountRecord,
    OperationUsage, OperationUse, ServiceState, UDisksBackend, UDisksError, UDisksRequest,
    UsageResolution, VolumeAction, VolumeChange, VolumeError, VolumeEvent, VolumeId, VolumeRuntime,
    VolumeService, VolumeSubscription, VolumeTrigger, ZbusUDisksBackend,
};
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct FakeBackend {
    snapshots: Mutex<VecDeque<Result<BackendSnapshot, UDisksError>>>,
    actions: Mutex<Vec<(VolumeId, VolumeAction)>>,
    action_results: Mutex<VecDeque<Result<(), UDisksError>>>,
    validation_results: Mutex<VecDeque<Result<(), UDisksError>>>,
}

impl FakeBackend {
    fn queue_snapshot(&self, snapshot: Result<BackendSnapshot, UDisksError>) {
        self.snapshots.lock().unwrap().push_back(snapshot);
    }

    fn queue_action(&self, result: Result<(), UDisksError>) {
        self.action_results.lock().unwrap().push_back(result);
    }

    fn queue_validation(&self, result: Result<(), UDisksError>) {
        self.validation_results.lock().unwrap().push_back(result);
    }
}

#[test]
fn action_revalidates_owner_and_object_identity_before_dispatch() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    backend.queue_snapshot(Ok(snapshot(
        "owner-before",
        [device("sdb1", Some("/media/a"))],
    )));
    mounts.queue(vec![mount("/dev/sdb1", "/media/a", false)]);
    let mut service = service(backend.clone(), mounts, Arc::new(FakeUsage::default()));
    service.refresh().unwrap();
    backend.queue_validation(Err(UDisksError::StaleObject));

    let error = service
        .perform(
            &id("sdb1"),
            VolumeAction::Unmount,
            UsageResolution::Refuse,
            None,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        VolumeError::StaleObject | VolumeError::Disappeared(_)
    ));
    assert!(backend.actions.lock().unwrap().is_empty());
}

impl UDisksBackend for FakeBackend {
    fn snapshot(&self) -> Result<BackendSnapshot, UDisksError> {
        self.snapshots
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(UDisksError::Unavailable("fixture exhausted".into())))
    }

    fn perform(
        &self,
        volume: &DeviceDescriptor,
        action: VolumeAction,
        _unlock_secret: Option<&str>,
    ) -> Result<(), UDisksError> {
        self.actions
            .lock()
            .unwrap()
            .push((volume.id().clone(), action));
        self.action_results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(()))
    }

    fn validate_action(
        &self,
        _volume: &DeviceDescriptor,
        _expected_owner: Option<&str>,
        _request: &UDisksRequest,
    ) -> Result<(), UDisksError> {
        self.validation_results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(()))
    }
}

#[derive(Default)]
struct FakeMounts {
    snapshots: Mutex<VecDeque<Result<Vec<MountRecord>, VolumeError>>>,
    capacities: Mutex<BTreeMap<PathBuf, Capacity>>,
}

impl FakeMounts {
    fn queue(&self, records: Vec<MountRecord>) {
        self.snapshots.lock().unwrap().push_back(Ok(records));
    }

    fn set_capacity(&self, path: impl Into<PathBuf>, total: u64, available: u64) {
        self.capacities
            .lock()
            .unwrap()
            .insert(path.into(), Capacity::new(total, available));
    }
}

impl MountProvider for FakeMounts {
    fn snapshot(&self) -> Result<Vec<MountRecord>, VolumeError> {
        self.snapshots
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    fn capacity(&self, path: &Path) -> Result<Capacity, VolumeError> {
        self.capacities
            .lock()
            .unwrap()
            .get(path)
            .copied()
            .ok_or_else(|| VolumeError::Capacity {
                path: path.to_path_buf(),
                reason: "missing fixture capacity".into(),
            })
    }
}

#[derive(Default)]
struct FakeUsage {
    active: Mutex<Vec<OperationUse>>,
    canceled: Mutex<Vec<MountOperation>>,
}

impl OperationUsage for FakeUsage {
    fn operations_using(&self, _mounts: &[PathBuf]) -> Vec<OperationUse> {
        self.active.lock().unwrap().clone()
    }

    fn cancel(&self, operations: &[OperationUse]) -> Result<(), VolumeError> {
        self.canceled
            .lock()
            .unwrap()
            .extend(operations.iter().map(|operation| operation.id()));
        self.active.lock().unwrap().clear();
        Ok(())
    }
}

fn id(value: &str) -> VolumeId {
    VolumeId::new(value).unwrap()
}

fn device(value: &str, mount: Option<&str>) -> DeviceDescriptor {
    DeviceDescriptor::new(
        id(value),
        format!("/org/freedesktop/UDisks2/block_devices/{value}"),
    )
    .with_label(format!("Disk {value}"))
    .with_device(format!("/dev/{value}"))
    .with_drive_path(format!("/org/freedesktop/UDisks2/drives/{value}"))
    .with_mount_points(mount.into_iter().map(PathBuf::from))
    .with_capabilities(true, true, true, true, true)
}

fn snapshot(owner: &str, devices: impl IntoIterator<Item = DeviceDescriptor>) -> BackendSnapshot {
    BackendSnapshot::new(owner, devices)
}

fn mount(source: &str, destination: &str, read_only: bool) -> MountRecord {
    MountRecord::new(source, destination, "ext4", read_only)
}

fn generic_mount(source: &str, destination: &str, filesystem: &str) -> MountRecord {
    MountRecord::new(source, destination, filesystem, false)
}

fn service(
    backend: Arc<FakeBackend>,
    mounts: Arc<FakeMounts>,
    usage: Arc<FakeUsage>,
) -> VolumeService {
    VolumeService::new(backend, mounts, usage)
}

#[test]
fn refresh_inserts_removes_and_deduplicates_mount_records() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    let usage = Arc::new(FakeUsage::default());
    backend.queue_snapshot(Ok(snapshot("owner-1", [device("sdb1", Some("/media/a"))])));
    mounts.queue(vec![
        mount("/dev/sdb1", "/media/a", false),
        mount("/dev/sdb1", "/media/a", false),
    ]);
    mounts.set_capacity("/media/a", 1_000, 400);
    let mut service = service(backend.clone(), mounts.clone(), usage);

    let first = service.refresh().unwrap();
    assert_eq!(service.model().volumes().len(), 1);
    let volume = service.model().get(&id("sdb1")).unwrap();
    assert_eq!(volume.mount_points(), &[PathBuf::from("/media/a")]);
    assert_eq!(volume.capacity(), Some(Capacity::new(1_000, 400)));
    assert_eq!(first.events(), &[VolumeEvent::Added(id("sdb1"))]);

    backend.queue_snapshot(Ok(snapshot("owner-1", [])));
    mounts.queue(Vec::new());
    let second = service.handle(VolumeTrigger::UDisksChanged).unwrap();
    assert!(service.model().volumes().is_empty());
    assert_eq!(second.events(), &[VolumeEvent::Removed(id("sdb1"))]);
}

#[test]
fn every_supported_volume_action_routes_through_the_backend_and_refreshes() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    let usage = Arc::new(FakeUsage::default());
    backend.queue_snapshot(Ok(snapshot("owner", [device("sdb1", None)])));
    mounts.queue(Vec::new());
    let mut service = service(backend.clone(), mounts.clone(), usage);
    service.refresh().unwrap();
    let unmounted = service.model().get(&id("sdb1")).unwrap().capabilities();
    assert!(unmounted.can_mount);
    assert!(!unmounted.can_unmount);

    for action in [
        VolumeAction::Mount,
        VolumeAction::Unlock,
        VolumeAction::Unmount,
        VolumeAction::Eject,
        VolumeAction::PowerOff,
    ] {
        if action == VolumeAction::Unmount {
            backend.queue_snapshot(Ok(snapshot("owner", [device("sdb1", Some("/media/a"))])));
            mounts.queue(vec![mount("/dev/sdb1", "/media/a", false)]);
            service.refresh().unwrap();
            let capabilities = service.model().get(&id("sdb1")).unwrap().capabilities();
            assert!(!capabilities.can_mount);
            assert!(capabilities.can_unmount);
        }
        backend.queue_action(Ok(()));
        backend.queue_snapshot(Ok(snapshot("owner", [device("sdb1", None)])));
        mounts.queue(Vec::new());
        let secret = (action == VolumeAction::Unlock).then_some("not-logged");
        service
            .perform(&id("sdb1"), action, UsageResolution::Refuse, secret)
            .unwrap();
    }

    assert_eq!(
        backend
            .actions
            .lock()
            .unwrap()
            .iter()
            .map(|(_, action)| *action)
            .collect::<Vec<_>>(),
        vec![
            VolumeAction::Mount,
            VolumeAction::Unlock,
            VolumeAction::Unmount,
            VolumeAction::Eject,
            VolumeAction::PowerOff,
        ]
    );
}

#[test]
fn busy_authorization_and_unsupported_errors_remain_typed() {
    for (backend_error, expected) in [
        (
            UDisksError::Busy("device is busy".into()),
            VolumeError::Busy("device is busy".into()),
        ),
        (
            UDisksError::AuthorizationRequired("authentication required".into()),
            VolumeError::AuthorizationRequired("authentication required".into()),
        ),
        (
            UDisksError::Unsupported("cannot eject".into()),
            VolumeError::Unsupported("cannot eject".into()),
        ),
    ] {
        let backend = Arc::new(FakeBackend::default());
        let mounts = Arc::new(FakeMounts::default());
        backend.queue_snapshot(Ok(snapshot("owner", [device("sdb1", Some("/media/a"))])));
        mounts.queue(vec![mount("/dev/sdb1", "/media/a", false)]);
        mounts.set_capacity("/media/a", 10, 5);
        let mut service = service(backend.clone(), mounts, Arc::new(FakeUsage::default()));
        service.refresh().unwrap();
        backend.queue_action(Err(backend_error));
        assert_eq!(
            service
                .perform(
                    &id("sdb1"),
                    VolumeAction::Eject,
                    UsageResolution::Refuse,
                    None,
                )
                .unwrap_err(),
            expected
        );
    }
}

#[test]
fn stale_objects_and_disappearance_during_an_operation_are_safe() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    backend.queue_snapshot(Ok(snapshot("owner", [device("sdb1", Some("/media/a"))])));
    mounts.queue(vec![mount("/dev/sdb1", "/media/a", false)]);
    mounts.set_capacity("/media/a", 10, 5);
    let mut service = service(
        backend.clone(),
        mounts.clone(),
        Arc::new(FakeUsage::default()),
    );
    service.refresh().unwrap();

    backend.queue_action(Err(UDisksError::StaleObject));
    backend.queue_snapshot(Ok(snapshot("owner", [])));
    mounts.queue(Vec::new());
    assert_eq!(
        service
            .perform(
                &id("sdb1"),
                VolumeAction::Unmount,
                UsageResolution::Refuse,
                None,
            )
            .unwrap_err(),
        VolumeError::Disappeared(id("sdb1"))
    );

    backend.queue_snapshot(Ok(snapshot("owner", [device("sdc1", Some("/media/b"))])));
    mounts.queue(vec![mount("/dev/sdc1", "/media/b", false)]);
    mounts.set_capacity("/media/b", 20, 10);
    service.refresh().unwrap();
    backend.queue_action(Ok(()));
    backend.queue_snapshot(Ok(snapshot("owner", [])));
    mounts.queue(Vec::new());
    let outcome = service
        .perform(
            &id("sdc1"),
            VolumeAction::Eject,
            UsageResolution::Refuse,
            None,
        )
        .unwrap();
    assert!(!outcome.volume_present());
}

#[test]
fn capability_capacity_and_read_only_changes_are_emitted() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    let first_device = device("sdb1", Some("/media/a"));
    backend.queue_snapshot(Ok(snapshot("owner", [first_device])));
    mounts.queue(vec![mount("/dev/sdb1", "/media/a", false)]);
    mounts.set_capacity("/media/a", 1_000, 400);
    let mut service = service(
        backend.clone(),
        mounts.clone(),
        Arc::new(FakeUsage::default()),
    );
    service.refresh().unwrap();

    let changed_device =
        device("sdb1", Some("/media/a")).with_capabilities(false, true, false, false, false);
    backend.queue_snapshot(Ok(snapshot("owner", [changed_device])));
    mounts.queue(vec![mount("/dev/sdb1", "/media/a", true)]);
    mounts.set_capacity("/media/a", 1_000, 100);
    let report = service.handle(VolumeTrigger::MountTableChanged).unwrap();
    let VolumeEvent::Changed { changes, .. } = &report.events()[0] else {
        panic!("expected a changed event");
    };
    assert!(changes.contains(VolumeChange::Capabilities));
    assert!(changes.contains(VolumeChange::Capacity));
    assert!(changes.contains(VolumeChange::ReadOnly));
}

#[test]
fn app_owned_operation_use_requires_refusal_or_explicit_cancellation() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    let usage = Arc::new(FakeUsage::default());
    usage
        .active
        .lock()
        .unwrap()
        .push(OperationUse::new(MountOperation::new(42), "copying photos"));
    backend.queue_snapshot(Ok(snapshot("owner", [device("sdb1", Some("/media/a"))])));
    mounts.queue(vec![mount("/dev/sdb1", "/media/a", false)]);
    mounts.set_capacity("/media/a", 10, 5);
    let mut service = service(backend.clone(), mounts.clone(), usage.clone());
    service.refresh().unwrap();

    let error = service
        .perform(
            &id("sdb1"),
            VolumeAction::Unmount,
            UsageResolution::Refuse,
            None,
        )
        .unwrap_err();
    assert!(matches!(error, VolumeError::InUse(ref active) if active.len() == 1));
    assert!(backend.actions.lock().unwrap().is_empty());

    backend.queue_action(Ok(()));
    backend.queue_snapshot(Ok(snapshot("owner", [device("sdb1", None)])));
    mounts.queue(Vec::new());
    service
        .perform(
            &id("sdb1"),
            VolumeAction::Unmount,
            UsageResolution::CancelApproved(vec![OperationUse::new(
                MountOperation::new(42),
                "copying photos",
            )]),
            None,
        )
        .unwrap();
    assert_eq!(
        usage.canceled.lock().unwrap().as_slice(),
        &[MountOperation::new(42)]
    );
}

#[test]
fn cancellation_consent_never_applies_to_operations_started_after_review() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    let usage = Arc::new(FakeUsage::default());
    let reviewed = OperationUse::new(MountOperation::new(42), "copying photos");
    usage.active.lock().unwrap().push(reviewed.clone());
    backend.queue_snapshot(Ok(snapshot("owner", [device("sdb1", Some("/media/a"))])));
    mounts.queue(vec![mount("/dev/sdb1", "/media/a", false)]);
    mounts.set_capacity("/media/a", 10, 5);
    let mut service = service(backend.clone(), mounts, usage.clone());
    service.refresh().unwrap();

    usage
        .active
        .lock()
        .unwrap()
        .push(OperationUse::new(MountOperation::new(43), "new write"));
    let error = service
        .perform(
            &id("sdb1"),
            VolumeAction::Unmount,
            UsageResolution::CancelApproved(vec![reviewed]),
            None,
        )
        .unwrap_err();

    assert!(matches!(error, VolumeError::InUse(ref active) if active.len() == 2));
    assert!(usage.canceled.lock().unwrap().is_empty());
    assert!(backend.actions.lock().unwrap().is_empty());
}

#[test]
fn mount_only_identity_survives_remount_and_groups_bind_mounts() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    let mut service = service(
        backend.clone(),
        mounts.clone(),
        Arc::new(FakeUsage::default()),
    );
    backend.queue_snapshot(Err(UDisksError::Unavailable("absent".into())));
    mounts.queue(vec![
        mount("/dev/mapper/data", "/media/old", false),
        mount("/dev/mapper/data", "/srv/data", false),
    ]);
    let first = service.refresh().unwrap();
    assert_eq!(service.model().volumes().len(), 1);
    let stable_id = service.model().volumes()[0].id().clone();
    assert_eq!(first.events(), &[VolumeEvent::Added(stable_id.clone())]);

    backend.queue_snapshot(Err(UDisksError::Unavailable("absent".into())));
    mounts.queue(vec![mount("/dev/mapper/data", "/media/new", false)]);
    let second = service.refresh().unwrap();
    assert_eq!(service.model().volumes()[0].id(), &stable_id);
    assert!(matches!(second.events(), [VolumeEvent::Changed { id, .. }] if id == &stable_id));
}

#[test]
fn equal_generic_mount_sources_remain_distinct_and_stable_across_remount() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    let mut service = service(
        backend.clone(),
        mounts.clone(),
        Arc::new(FakeUsage::default()),
    );
    backend.queue_snapshot(Err(UDisksError::Unavailable("absent".into())));
    mounts.queue(vec![
        generic_mount("tmpfs", "/run/first", "tmpfs"),
        generic_mount("tmpfs", "/run/second", "tmpfs"),
    ]);
    service.refresh().unwrap();
    let first_ids = service
        .model()
        .volumes()
        .into_iter()
        .map(|volume| volume.id().clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(first_ids.len(), 2, "equal generic sources must not merge");

    backend.queue_snapshot(Err(UDisksError::Unavailable("absent".into())));
    mounts.queue(vec![
        generic_mount("tmpfs", "/run/remounted-first", "tmpfs"),
        generic_mount("tmpfs", "/run/remounted-second", "tmpfs"),
    ]);
    service.refresh().unwrap();
    let remounted_ids = service
        .model()
        .volumes()
        .into_iter()
        .map(|volume| volume.id().clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(remounted_ids, first_ids);
}

#[test]
fn generic_mount_reconciliation_never_overwrites_a_new_identity_collision() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    let mut service = service(
        backend.clone(),
        mounts.clone(),
        Arc::new(FakeUsage::default()),
    );
    backend.queue_snapshot(Err(UDisksError::Unavailable("absent".into())));
    mounts.queue(vec![
        generic_mount("tmpfs", "/run/a", "tmpfs"),
        generic_mount("tmpfs", "/run/z", "tmpfs"),
    ]);
    service.refresh().unwrap();
    let initial = service
        .model()
        .volumes()
        .into_iter()
        .map(|volume| (volume.mount_points()[0].clone(), volume.id().clone()))
        .collect::<BTreeMap<_, _>>();
    let a_id = initial[Path::new("/run/a")].clone();
    let z_id = initial[Path::new("/run/z")].clone();

    backend.queue_snapshot(Err(UDisksError::Unavailable("absent".into())));
    mounts.queue(vec![
        generic_mount("tmpfs", "/run/b", "tmpfs"),
        generic_mount("tmpfs", "/run/z", "tmpfs"),
    ]);
    service.refresh().unwrap();

    assert_eq!(service.model().volumes().len(), 2);
    let by_destination = service
        .model()
        .volumes()
        .into_iter()
        .map(|volume| (volume.mount_points()[0].clone(), volume.id().clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(by_destination[Path::new("/run/z")], z_id);
    assert_ne!(by_destination[Path::new("/run/b")], a_id);
    assert_ne!(by_destination[Path::new("/run/b")], z_id);
    assert_eq!(
        service
            .model()
            .volumes()
            .into_iter()
            .flat_map(|volume| volume.mount_points())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2
    );
}

#[test]
fn temporary_udisks_absence_preserves_the_known_device_identity() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    let mut service = service(
        backend.clone(),
        mounts.clone(),
        Arc::new(FakeUsage::default()),
    );
    backend.queue_snapshot(Ok(snapshot("owner", [device("sdb1", Some("/media/a"))])));
    mounts.queue(vec![mount("/dev/sdb1", "/media/a", false)]);
    service.refresh().unwrap();

    backend.queue_snapshot(Err(UDisksError::Unavailable("restarting".into())));
    mounts.queue(vec![mount("/dev/sdb1", "/media/a", false)]);
    service.refresh().unwrap();

    assert!(service.model().get(&id("sdb1")).is_some());
    assert_eq!(service.model().volumes().len(), 1);
}

#[test]
fn duplicate_device_id_is_rejected_without_overwriting_the_model() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    let mut service = service(
        backend.clone(),
        mounts.clone(),
        Arc::new(FakeUsage::default()),
    );
    backend.queue_snapshot(Ok(snapshot("owner", [device("sdb1", None)])));
    mounts.queue(Vec::new());
    service.refresh().unwrap();

    let duplicate = DeviceDescriptor::new(id("clone"), "/org/test/one")
        .with_device("/dev/one")
        .with_capabilities(true, false, false, false, false);
    backend.queue_snapshot(Ok(snapshot(
        "owner",
        [
            duplicate.clone(),
            DeviceDescriptor::new(id("clone"), "/org/test/two")
                .with_device("/dev/two")
                .with_capabilities(true, false, false, false, false),
        ],
    )));
    mounts.queue(Vec::new());
    assert!(matches!(service.refresh(), Err(VolumeError::Protocol(_))));
    assert!(service.model().get(&id("sdb1")).is_some());
    assert!(service.model().get(&id("clone")).is_none());
}

#[test]
fn absent_slow_disconnected_and_restarted_services_keep_mounts_usable() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    let mut service = service(
        backend.clone(),
        mounts.clone(),
        Arc::new(FakeUsage::default()),
    );

    for (error, expected_state) in [
        (
            UDisksError::Unavailable("service absent".into()),
            ServiceState::Unavailable,
        ),
        (
            UDisksError::Timeout("service slow".into()),
            ServiceState::Slow,
        ),
        (
            UDisksError::Disconnected("bus disconnected".into()),
            ServiceState::Disconnected,
        ),
    ] {
        backend.queue_snapshot(Err(error));
        mounts.queue(vec![mount("/dev/sdb1", "/media/a", false)]);
        mounts.set_capacity("/media/a", 10, 5);
        let report = service.refresh().unwrap();
        assert_eq!(report.service_state(), expected_state);
        assert_eq!(service.model().volumes().len(), 1);
        assert!(service.model().volumes()[0].is_mounted());
    }

    backend.queue_snapshot(Ok(snapshot(
        "owner-after-restart",
        [device("sdb1", Some("/media/a")), device("sdc1", None)],
    )));
    mounts.queue(vec![mount("/dev/sdb1", "/media/a", false)]);
    let report = service.handle(VolumeTrigger::ServiceOwnerChanged).unwrap();
    assert_eq!(report.service_state(), ServiceState::Available);
    assert_eq!(service.model().service_owner(), Some("owner-after-restart"));
    assert_eq!(service.model().volumes().len(), 2);
}

#[test]
fn udisks_smoke_is_bounded_and_optional_on_unsupported_hosts() {
    let started = Instant::now();
    let Ok(backend) = ZbusUDisksBackend::connect_system_with_timeout(Duration::from_secs(2)) else {
        return;
    };
    match backend.snapshot() {
        Ok(snapshot) => assert!(!snapshot.owner().is_empty()),
        Err(UDisksError::Unavailable(_) | UDisksError::Disconnected(_)) => {}
        Err(error) => panic!("unexpected UDisks2 smoke-test error: {error}"),
    }
    assert!(started.elapsed() < Duration::from_secs(5));
}

struct SlowBackend;

impl UDisksBackend for SlowBackend {
    fn snapshot(&self) -> Result<BackendSnapshot, UDisksError> {
        std::thread::sleep(Duration::from_millis(250));
        Err(UDisksError::Timeout("fixture deadline".into()))
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

#[test]
fn runtime_snapshots_never_wait_for_a_slow_service_call() {
    let runtime = VolumeRuntime::from_service(VolumeService::new(
        Arc::new(SlowBackend),
        Arc::new(FakeMounts::default()),
        Arc::new(FakeUsage::default()),
    ));
    let updates = runtime.subscribe();
    let _initial = updates.recv_blocking().unwrap();
    runtime.refresh(VolumeTrigger::UDisksChanged).unwrap();
    std::thread::sleep(Duration::from_millis(25));

    let started = Instant::now();
    let _snapshot = runtime.snapshot();
    assert!(started.elapsed() < Duration::from_millis(25));
    let update = updates.recv_blocking().unwrap();
    assert_eq!(
        update.warning(),
        Some(&VolumeError::Timeout("fixture deadline".into()))
    );
}

#[test]
fn runtime_shutdown_joins_worker_and_releases_service_dependencies() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    let backend_weak = Arc::downgrade(&backend);
    let mounts_weak = Arc::downgrade(&mounts);
    let runtime = VolumeRuntime::from_service(VolumeService::new(
        backend.clone(),
        mounts.clone(),
        Arc::new(FakeUsage::default()),
    ));
    drop(backend);
    drop(mounts);
    drop(runtime);
    assert!(backend_weak.upgrade().is_none());
    assert!(mounts_weak.upgrade().is_none());
}

struct CooperativeSlowBackend;

impl UDisksBackend for CooperativeSlowBackend {
    fn snapshot(&self) -> Result<BackendSnapshot, UDisksError> {
        unreachable!("runtime must use the request-bounded entry point")
    }

    fn snapshot_with_request(
        &self,
        request: &UDisksRequest,
    ) -> Result<BackendSnapshot, UDisksError> {
        loop {
            request.check()?;
            std::thread::sleep(Duration::from_millis(5));
        }
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

#[test]
fn listener_enabled_shutdown_preempts_a_slow_snapshot_and_queued_refreshes() {
    let (sender, receiver) = async_channel::bounded(32);
    let listener = std::thread::spawn(move || {
        while !sender.is_closed() {
            let _ = sender.try_send(VolumeTrigger::UDisksChanged);
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let subscription = VolumeSubscription::from_parts(receiver, vec![listener]);
    let runtime = VolumeRuntime::from_service_with_subscription(
        VolumeService::new(
            Arc::new(CooperativeSlowBackend),
            Arc::new(FakeMounts::default()),
            Arc::new(FakeUsage::default()),
        ),
        subscription,
    );
    std::thread::sleep(Duration::from_millis(20));

    let started = Instant::now();
    drop(runtime);
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "shutdown must cancel an in-flight snapshot instead of draining refresh FIFO"
    );
}

#[derive(Default)]
struct ScriptedUsage {
    responses: Mutex<VecDeque<Vec<OperationUse>>>,
    queried_mounts: Mutex<Vec<Vec<PathBuf>>>,
}

impl OperationUsage for ScriptedUsage {
    fn operations_using(&self, mounts: &[PathBuf]) -> Vec<OperationUse> {
        self.queried_mounts.lock().unwrap().push(mounts.to_vec());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default()
    }

    fn cancel(&self, _operations: &[OperationUse]) -> Result<(), VolumeError> {
        Ok(())
    }
}

#[test]
fn drive_wide_actions_include_sibling_partition_jobs() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    let usage = Arc::new(ScriptedUsage::default());
    usage
        .responses
        .lock()
        .unwrap()
        .push_back(vec![OperationUse::new(
            MountOperation::new(9),
            "writing sibling",
        )]);
    let drive = "/org/freedesktop/UDisks2/drives/shared";
    let first = device("sdb1", Some("/media/a")).with_drive_path(drive);
    let second = device("sdb2", Some("/media/b")).with_drive_path(drive);
    backend.queue_snapshot(Ok(snapshot("owner", [first, second])));
    mounts.queue(vec![
        mount("/dev/sdb1", "/media/a", false),
        mount("/dev/sdb2", "/media/b", false),
    ]);
    let mut service = VolumeService::new(backend.clone(), mounts, usage.clone());
    service.refresh().unwrap();

    let error = service
        .perform(
            &id("sdb1"),
            VolumeAction::Eject,
            UsageResolution::Refuse,
            None,
        )
        .unwrap_err();
    assert!(matches!(error, VolumeError::InUse(_)));
    assert_eq!(
        usage.queried_mounts.lock().unwrap()[0],
        vec![PathBuf::from("/media/a"), PathBuf::from("/media/b")]
    );
    assert!(backend.actions.lock().unwrap().is_empty());
}

#[test]
fn cancellation_waits_and_rechecks_for_new_jobs_before_dispatch() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    let usage = Arc::new(ScriptedUsage::default());
    let approved = OperationUse::new(MountOperation::new(10), "copying");
    let replacement = OperationUse::new(MountOperation::new(11), "new write");
    usage.responses.lock().unwrap().extend([
        vec![approved.clone()],
        vec![approved.clone()],
        vec![replacement.clone()],
    ]);
    backend.queue_snapshot(Ok(snapshot("owner", [device("sdb1", Some("/media/a"))])));
    mounts.queue(vec![mount("/dev/sdb1", "/media/a", false)]);
    let mut service = VolumeService::new(backend.clone(), mounts, usage);
    service.refresh().unwrap();

    let error = service
        .perform(
            &id("sdb1"),
            VolumeAction::Unmount,
            UsageResolution::CancelApproved(vec![approved]),
            None,
        )
        .unwrap_err();
    assert!(matches!(error, VolumeError::InUse(ref jobs) if jobs == &[replacement]));
    assert!(backend.actions.lock().unwrap().is_empty());
}

struct BlockingCapacity {
    snapshots: Mutex<VecDeque<Result<Vec<MountRecord>, VolumeError>>>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl MountProvider for BlockingCapacity {
    fn snapshot(&self) -> Result<Vec<MountRecord>, VolumeError> {
        self.snapshots
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(Vec::new()))
    }

    fn capacity(&self, _path: &Path) -> Result<Capacity, VolumeError> {
        let _ = self.release.lock().unwrap().recv();
        Ok(Capacity::new(1, 1))
    }
}

#[test]
fn listener_shutdown_is_not_blocked_by_a_stalled_capacity_probe() {
    let backend = Arc::new(FakeBackend::default());
    backend.queue_snapshot(Ok(snapshot("owner", [device("sdb1", Some("/hang"))])));
    let (release, blocked) = std::sync::mpsc::channel();
    let provider = Arc::new(BlockingCapacity {
        snapshots: Mutex::new(VecDeque::from([Ok(vec![mount(
            "/dev/sdb1",
            "/hang",
            false,
        )])])),
        release: Mutex::new(blocked),
    });
    let (events, receiver) = async_channel::bounded(1);
    events.try_send(VolumeTrigger::MountTableChanged).unwrap();
    let runtime = VolumeRuntime::from_service_with_subscription(
        VolumeService::new(backend, provider, Arc::new(FakeUsage::default())),
        VolumeSubscription::from_parts(receiver, Vec::new()),
    );
    std::thread::sleep(Duration::from_millis(25));
    let started = Instant::now();
    drop(runtime);
    assert!(started.elapsed() < Duration::from_millis(250));
    release.send(()).unwrap();
}

#[test]
fn subscriber_backpressure_replaces_stale_updates_with_the_latest_state() {
    let backend = Arc::new(FakeBackend::default());
    let mounts = Arc::new(FakeMounts::default());
    for index in 0..12 {
        backend.queue_snapshot(Ok(snapshot(
            "owner",
            [device(&format!("sdb{index}"), None)],
        )));
        mounts.queue(Vec::new());
    }
    backend.queue_snapshot(Ok(snapshot("owner", [])));
    mounts.queue(Vec::new());
    let runtime = VolumeRuntime::from_service(VolumeService::new(
        backend,
        mounts,
        Arc::new(FakeUsage::default()),
    ));
    let updates = runtime.subscribe();
    for _ in 0..13 {
        runtime.refresh(VolumeTrigger::UDisksChanged).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while !runtime.snapshot().volumes().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut last = updates.recv_blocking().unwrap();
    while let Ok(update) = updates.try_recv() {
        last = update;
    }
    assert!(last.model().volumes().is_empty());
}

struct CurrentBackend(Mutex<BackendSnapshot>);

impl UDisksBackend for CurrentBackend {
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

#[test]
fn event_overflow_still_reconciles_the_final_device_removal() {
    let backend = Arc::new(CurrentBackend(Mutex::new(snapshot(
        "owner",
        [device("sdb1", None)],
    ))));
    let (sender, receiver) = async_channel::bounded(32);
    let runtime = VolumeRuntime::from_service_with_subscription(
        VolumeService::new(
            backend.clone(),
            Arc::new(FakeMounts::default()),
            Arc::new(FakeUsage::default()),
        ),
        VolumeSubscription::from_parts(receiver, Vec::new()),
    );
    sender.try_send(VolumeTrigger::UDisksChanged).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while runtime.snapshot().get(&id("sdb1")).is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    *backend.0.lock().unwrap() = snapshot("owner", []);
    for _ in 0..32 {
        let _ = sender.try_send(VolumeTrigger::UDisksChanged);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while !runtime.snapshot().volumes().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(runtime.snapshot().volumes().is_empty());
}
