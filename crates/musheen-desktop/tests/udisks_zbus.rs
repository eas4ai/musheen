#![cfg(unix)]

use musheen_desktop::{
    Capacity, MountProvider, MountRecord, NoOperationUsage, UDisksBackend, UDisksBusConfig,
    UDisksError, UDisksRequest, VolumeAction, VolumeError, VolumeRuntime, VolumeService,
    VolumeSubscription, VolumeTrigger, ZbusUDisksBackend,
};
use std::collections::HashMap;
use std::io::{BufRead as _, BufReader};
use std::os::unix::net::UnixListener;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use zbus::fdo::ObjectManager;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const SERVICE: &str = "org.freedesktop.UDisks2";
const ROOT: &str = "/org/freedesktop/UDisks2";
const BLOCK_PATH: &str = "/org/freedesktop/UDisks2/block_devices/fake1";
const ADDED_PATH: &str = "/org/freedesktop/UDisks2/block_devices/added";
const DRIVE_PATH: &str = "/org/freedesktop/UDisks2/drives/fake";
static SLOW_PROPERTIES_ENTERED: AtomicUsize = AtomicUsize::new(0);

struct EmptyMounts;

impl MountProvider for EmptyMounts {
    fn snapshot(&self) -> Result<Vec<MountRecord>, VolumeError> {
        Ok(Vec::new())
    }
    fn capacity(&self, path: &std::path::Path) -> Result<Capacity, VolumeError> {
        Err(VolumeError::Capacity {
            path: path.to_path_buf(),
            reason: "unused".into(),
        })
    }
}

struct PrivateBus {
    child: Child,
    address: String,
}

impl PrivateBus {
    fn start() -> Self {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("private D-Bus tests require dbus-daemon");
        let mut address = String::new();
        BufReader::new(child.stdout.take().expect("dbus-daemon stdout is piped"))
            .read_line(&mut address)
            .expect("dbus-daemon must report its address");
        assert!(
            !address.trim().is_empty(),
            "dbus-daemon returned no address"
        );
        Self {
            child,
            address: address.trim().to_owned(),
        }
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct FakeBlock {
    slow: Arc<AtomicBool>,
}

struct IdentityBlock {
    device: Vec<u8>,
    uuid: &'static str,
    drive: &'static str,
    symlinks: Vec<Vec<u8>>,
}

#[zbus::interface(name = "org.freedesktop.UDisks2.Block")]
impl IdentityBlock {
    #[zbus(property)]
    fn device(&self) -> Vec<u8> {
        self.device.clone()
    }
    #[zbus(property)]
    fn symlinks(&self) -> Vec<Vec<u8>> {
        self.symlinks.clone()
    }
    #[zbus(property)]
    fn id_uuid(&self) -> &str {
        self.uuid
    }
    #[zbus(property)]
    fn id_label(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn size(&self) -> u64 {
        4096
    }
    #[zbus(property)]
    fn read_only(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn drive(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(self.drive).unwrap()
    }
}

struct IdentityDrive {
    identity: &'static str,
}

#[zbus::interface(name = "org.freedesktop.UDisks2.Drive")]
impl IdentityDrive {
    #[zbus(property)]
    fn ejectable(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_power_off(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn wwn(&self) -> &str {
        self.identity
    }
    #[zbus(property)]
    fn serial(&self) -> &str {
        self.identity
    }
}

struct FakePartition {
    uuid: &'static str,
    number: u32,
    offset: u64,
}

#[zbus::interface(name = "org.freedesktop.UDisks2.Partition")]
impl FakePartition {
    #[zbus(property)]
    fn uuid(&self) -> &str {
        self.uuid
    }
    #[zbus(property)]
    fn number(&self) -> u32 {
        self.number
    }
    #[zbus(property)]
    fn offset(&self) -> u64 {
        self.offset
    }
}

#[zbus::interface(name = "org.freedesktop.UDisks2.Block")]
impl FakeBlock {
    fn pause(&self) {
        if self.slow.load(Ordering::SeqCst) {
            SLOW_PROPERTIES_ENTERED.fetch_add(1, Ordering::AcqRel);
            std::thread::sleep(Duration::from_millis(80));
        }
    }

    #[zbus(property)]
    fn device(&self) -> Vec<u8> {
        self.pause();
        b"/dev/fake1\0".to_vec()
    }

    #[zbus(property)]
    fn symlinks(&self) -> Vec<Vec<u8>> {
        self.pause();
        vec![b"/dev/disk/by-id/fake-drive\0".to_vec()]
    }

    #[zbus(property)]
    fn id_uuid(&self) -> &str {
        self.pause();
        "cloneable-uuid"
    }

    #[zbus(property)]
    fn id_label(&self) -> &str {
        self.pause();
        "Fake disk"
    }

    #[zbus(property)]
    fn size(&self) -> u64 {
        self.pause();
        4096
    }

    #[zbus(property)]
    fn read_only(&self) -> bool {
        self.pause();
        false
    }

    #[zbus(property)]
    fn drive(&self) -> OwnedObjectPath {
        self.pause();
        OwnedObjectPath::try_from(DRIVE_PATH).unwrap()
    }
}

struct FakeFilesystem {
    unmount_calls: Arc<AtomicUsize>,
    error_mode: Arc<AtomicUsize>,
    actions: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.freedesktop.UDisks2.Filesystem")]
impl FakeFilesystem {
    #[zbus(property)]
    fn mount_points(&self) -> Vec<Vec<u8>> {
        vec![b"/media/fake\0".to_vec()]
    }

    fn mount(&self, options: HashMap<String, OwnedValue>) -> zbus::fdo::Result<String> {
        assert!(options.is_empty());
        self.actions.lock().unwrap().push("mount".into());
        Ok("/media/fake".into())
    }

    fn unmount(&self, options: HashMap<String, OwnedValue>) -> zbus::fdo::Result<()> {
        assert!(
            options.is_empty(),
            "UDisks options must be explicit and empty"
        );
        self.unmount_calls.fetch_add(1, Ordering::SeqCst);
        self.actions.lock().unwrap().push("unmount".into());
        match self.error_mode.load(Ordering::SeqCst) {
            1 => Err(zbus::fdo::Error::Failed("DeviceBusy fixture".into())),
            2 => Err(zbus::fdo::Error::Failed("NotAuthorized fixture".into())),
            3 => Err(zbus::fdo::Error::Failed("NotSupported fixture".into())),
            4 => Err(zbus::fdo::Error::Failed("UnknownObject fixture".into())),
            _ => Ok(()),
        }
    }
}

struct FakeDrive {
    actions: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.freedesktop.UDisks2.Drive")]
impl FakeDrive {
    #[zbus(property)]
    fn ejectable(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn can_power_off(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn wwn(&self) -> &str {
        "wwn-fake"
    }
    #[zbus(property)]
    fn serial(&self) -> &str {
        "serial-fake"
    }
    fn eject(&self, options: HashMap<String, OwnedValue>) {
        assert!(options.is_empty());
        self.actions.lock().unwrap().push("eject".into());
    }
    fn power_off(&self, options: HashMap<String, OwnedValue>) {
        assert!(options.is_empty());
        self.actions.lock().unwrap().push("power-off".into());
    }
}

struct FakeEncrypted {
    actions: Arc<Mutex<Vec<String>>>,
}

#[zbus::interface(name = "org.freedesktop.UDisks2.Encrypted")]
impl FakeEncrypted {
    #[zbus(property)]
    fn cleartext_device(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from("/").unwrap()
    }
    fn unlock(&self, secret: &str, options: HashMap<String, OwnedValue>) -> OwnedObjectPath {
        assert_eq!(secret, "secret");
        assert!(options.is_empty());
        self.actions.lock().unwrap().push("unlock".into());
        OwnedObjectPath::try_from(BLOCK_PATH).unwrap()
    }
}

fn start_service(
    address: &str,
    calls: Arc<AtomicUsize>,
    slow: Arc<AtomicBool>,
    error_mode: Arc<AtomicUsize>,
    actions: Arc<Mutex<Vec<String>>>,
) -> zbus::Connection {
    futures_lite::future::block_on(async {
        zbus::connection::Builder::address(address)
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(ROOT, ObjectManager)
            .unwrap()
            .serve_at(BLOCK_PATH, FakeBlock { slow })
            .unwrap()
            .serve_at(
                BLOCK_PATH,
                FakeEncrypted {
                    actions: actions.clone(),
                },
            )
            .unwrap()
            .serve_at(
                BLOCK_PATH,
                FakeFilesystem {
                    unmount_calls: calls,
                    error_mode,
                    actions: actions.clone(),
                },
            )
            .unwrap()
            .serve_at(DRIVE_PATH, FakeDrive { actions })
            .unwrap()
            .build()
            .await
            .unwrap()
    })
}

fn start_identity_service(
    address: &str,
    paths: [(&'static str, &'static [u8]); 3],
) -> zbus::Connection {
    futures_lite::future::block_on(async {
        let actions = Arc::new(Mutex::new(Vec::new()));
        let mut builder = zbus::connection::Builder::address(address)
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(ROOT, ObjectManager)
            .unwrap()
            .serve_at(DRIVE_PATH, FakeDrive { actions })
            .unwrap();
        for (index, (path, device)) in paths.into_iter().enumerate() {
            builder = builder
                .serve_at(
                    path,
                    IdentityBlock {
                        device: device.to_vec(),
                        uuid: if index == 2 { "" } else { "same-uuid" },
                        drive: DRIVE_PATH,
                        symlinks: Vec::new(),
                    },
                )
                .unwrap()
                .serve_at(
                    path,
                    FakePartition {
                        uuid: "",
                        number: u32::try_from(index + 1).unwrap(),
                        offset: u64::try_from(index + 1).unwrap() * 4096,
                    },
                )
                .unwrap();
        }
        builder.build().await.unwrap()
    })
}

fn start_duplicate_hardware_service(
    address: &str,
    hardware_identity: &'static str,
    partitions: bool,
) -> zbus::Connection {
    const DRIVE_A: &str = "/org/freedesktop/UDisks2/drives/duplicate_a";
    const DRIVE_B: &str = "/org/freedesktop/UDisks2/drives/duplicate_b";
    const BLOCK_A: &str = "/org/freedesktop/UDisks2/block_devices/duplicate_a1";
    const BLOCK_B: &str = "/org/freedesktop/UDisks2/block_devices/duplicate_b1";

    futures_lite::future::block_on(async {
        let mut builder = zbus::connection::Builder::address(address)
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(ROOT, ObjectManager)
            .unwrap()
            .serve_at(
                DRIVE_A,
                IdentityDrive {
                    identity: hardware_identity,
                },
            )
            .unwrap()
            .serve_at(
                DRIVE_B,
                IdentityDrive {
                    identity: hardware_identity,
                },
            )
            .unwrap()
            .serve_at(
                BLOCK_A,
                IdentityBlock {
                    device: b"/dev/sdb1\0".to_vec(),
                    uuid: "",
                    drive: DRIVE_A,
                    symlinks: Vec::new(),
                },
            )
            .unwrap()
            .serve_at(
                BLOCK_B,
                IdentityBlock {
                    device: b"/dev/sdc1\0".to_vec(),
                    uuid: "",
                    drive: DRIVE_B,
                    symlinks: Vec::new(),
                },
            )
            .unwrap();
        if partitions {
            for path in [BLOCK_A, BLOCK_B] {
                builder = builder
                    .serve_at(
                        path,
                        FakePartition {
                            uuid: "",
                            number: 1,
                            offset: 4096,
                        },
                    )
                    .unwrap();
            }
        }
        builder.build().await.unwrap()
    })
}

fn assert_duplicate_hardware_is_visible_but_unsafe(backend: ZbusUDisksBackend) {
    let snapshot = backend.snapshot().unwrap();
    assert_eq!(snapshot.devices().len(), 2);
    let ids = snapshot
        .devices()
        .iter()
        .map(|device| device.id().clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 2, "ambiguous devices need session-safe IDs");
    for device in snapshot.devices() {
        assert!(device.id().as_str().starts_with("device-ambiguous-"));
        let capabilities = device.capabilities();
        assert!(!capabilities.can_mount);
        assert!(!capabilities.can_unmount);
        assert!(!capabilities.can_eject);
        assert!(!capabilities.can_unlock);
        assert!(!capabilities.can_power_off);
    }

    let mut service = VolumeService::new(
        Arc::new(backend),
        Arc::new(EmptyMounts),
        Arc::new(NoOperationUsage),
    );
    service.refresh().unwrap();
    assert_eq!(service.model().volumes().len(), 2);
}

#[test]
fn empty_hardware_identity_does_not_trust_duplicate_partition_layouts() {
    let bus = PrivateBus::start();
    let _service = start_duplicate_hardware_service(&bus.address, "", true);
    let backend =
        ZbusUDisksBackend::connect_address_with_timeout(&bus.address, Duration::from_millis(500))
            .unwrap();
    assert_duplicate_hardware_is_visible_but_unsafe(backend);
}

#[test]
fn duplicated_serial_does_not_trust_whole_device_identity() {
    let bus = PrivateBus::start();
    let _service = start_duplicate_hardware_service(&bus.address, "duplicate-serial", false);
    let backend =
        ZbusUDisksBackend::connect_address_with_timeout(&bus.address, Duration::from_millis(500))
            .unwrap();
    assert_duplicate_hardware_is_visible_but_unsafe(backend);
}

#[test]
fn same_drive_partition_identities_survive_object_and_kernel_path_churn() {
    let bus = PrivateBus::start();
    let first_service = start_identity_service(
        &bus.address,
        [
            (
                "/org/freedesktop/UDisks2/block_devices/sdb1",
                b"/dev/sdb1\0",
            ),
            (
                "/org/freedesktop/UDisks2/block_devices/sdb2",
                b"/dev/sdb2\0",
            ),
            (
                "/org/freedesktop/UDisks2/block_devices/sdb3",
                b"/dev/sdb3\0",
            ),
        ],
    );
    let backend =
        ZbusUDisksBackend::connect_address_with_timeout(&bus.address, Duration::from_millis(500))
            .unwrap();
    let before = backend.snapshot().unwrap();
    assert_eq!(before.devices().len(), 3);
    let before_ids = before
        .devices()
        .iter()
        .map(|device| device.id().clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(before_ids.len(), 3);
    assert!(before.devices().iter().all(|device| {
        let capabilities = device.capabilities();
        capabilities.can_eject && capabilities.can_power_off
    }));

    drop(first_service);
    let second_service = start_identity_service(
        &bus.address,
        [
            (
                "/org/freedesktop/UDisks2/block_devices/sdz7",
                b"/dev/sdz7\0",
            ),
            (
                "/org/freedesktop/UDisks2/block_devices/sdz8",
                b"/dev/sdz8\0",
            ),
            (
                "/org/freedesktop/UDisks2/block_devices/sdz9",
                b"/dev/sdz9\0",
            ),
        ],
    );
    let after = backend.snapshot().unwrap();
    let after_ids = after
        .devices()
        .iter()
        .map(|device| device.id().clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(after_ids, before_ids);
    drop(second_service);
}

#[test]
fn fake_udisks_object_manager_properties_owner_restart_and_errors() {
    let mut bus = PrivateBus::start();
    let calls = Arc::new(AtomicUsize::new(0));
    let slow = Arc::new(AtomicBool::new(false));
    let error_mode = Arc::new(AtomicUsize::new(1));
    let actions = Arc::new(Mutex::new(Vec::new()));
    let first_service = start_service(
        &bus.address,
        Arc::clone(&calls),
        Arc::clone(&slow),
        Arc::clone(&error_mode),
        Arc::clone(&actions),
    );
    let backend =
        ZbusUDisksBackend::connect_address_with_timeout(&bus.address, Duration::from_millis(500))
            .unwrap();

    let first = backend.snapshot().unwrap();
    assert_eq!(first.devices().len(), 1);
    assert_eq!(first.devices()[0].label(), "Fake disk");
    assert_eq!(
        first.devices()[0].mount_points(),
        &[std::path::PathBuf::from("/media/fake")]
    );
    assert!(matches!(
        backend.perform(&first.devices()[0], VolumeAction::Unmount, None),
        Err(UDisksError::Busy(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    for (mode, expected) in [(2, "authorization"), (3, "unsupported"), (4, "stale")] {
        error_mode.store(mode, Ordering::SeqCst);
        let error = backend
            .perform(&first.devices()[0], VolumeAction::Unmount, None)
            .unwrap_err();
        assert!(
            matches!(
                (expected, error),
                ("authorization", UDisksError::AuthorizationRequired(_))
                    | ("unsupported", UDisksError::Unsupported(_))
                    | ("stale", UDisksError::StaleObject)
            ),
            "expected {expected}"
        );
    }
    error_mode.store(0, Ordering::SeqCst);
    for (action, secret) in [
        (VolumeAction::Mount, None),
        (VolumeAction::Unmount, None),
        (VolumeAction::Eject, None),
        (VolumeAction::Unlock, Some("secret")),
        (VolumeAction::PowerOff, None),
    ] {
        backend
            .perform(&first.devices()[0], action, secret)
            .unwrap();
    }
    assert_eq!(
        actions.lock().unwrap().as_slice(),
        [
            "unmount",
            "unmount",
            "unmount",
            "unmount",
            "mount",
            "unmount",
            "eject",
            "unlock",
            "power-off"
        ]
    );

    futures_lite::future::block_on(first_service.release_name(SERVICE)).unwrap();
    assert!(matches!(
        backend.snapshot(),
        Err(UDisksError::Unavailable(_))
    ));
    let second_service = start_service(
        &bus.address,
        Arc::clone(&calls),
        Arc::clone(&slow),
        Arc::clone(&error_mode),
        Arc::clone(&actions),
    );
    let restarted = backend.snapshot().unwrap();
    assert_ne!(first.owner(), restarted.owner());
    let actions_before = actions.lock().unwrap().len();
    let request = UDisksRequest::with_timeout(Duration::from_millis(500));
    assert_eq!(
        backend.perform_validated_with_request(
            &first.devices()[0],
            Some(first.owner()),
            VolumeAction::Unmount,
            None,
            &request,
        ),
        Err(UDisksError::StaleObject)
    );
    assert_eq!(actions.lock().unwrap().len(), actions_before);

    slow.store(true, Ordering::SeqCst);
    SLOW_PROPERTIES_ENTERED.store(0, Ordering::Release);
    let started = std::time::Instant::now();
    match backend.snapshot() {
        Err(UDisksError::DeadlineExceeded) => {}
        other => panic!("slow property must hit the production deadline, got {other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(second_service);

    bus.child.kill().unwrap();
    let _ = bus.child.wait();
    match backend.snapshot() {
        Err(
            UDisksError::Disconnected(_) | UDisksError::Timeout(_) | UDisksError::DeadlineExceeded,
        ) => {}
        other => panic!("dead private bus must be a connection error, got {other:?}"),
    }
}

#[test]
fn same_owner_replacement_at_the_same_object_path_is_rejected() {
    let bus = PrivateBus::start();
    let slow = Arc::new(AtomicBool::new(false));
    let service = start_service(
        &bus.address,
        Arc::new(AtomicUsize::new(0)),
        Arc::clone(&slow),
        Arc::new(AtomicUsize::new(0)),
        Arc::new(Mutex::new(Vec::new())),
    );
    let backend =
        ZbusUDisksBackend::connect_address_with_timeout(&bus.address, Duration::from_millis(500))
            .unwrap();
    let before = backend.snapshot().unwrap();
    futures_lite::future::block_on(async {
        service
            .object_server()
            .remove::<FakeBlock, _>(BLOCK_PATH)
            .await
            .unwrap();
        service
            .object_server()
            .at(
                BLOCK_PATH,
                IdentityBlock {
                    device: b"/dev/replacement\0".to_vec(),
                    uuid: "",
                    drive: DRIVE_PATH,
                    symlinks: vec![b"/dev/disk/by-id/fake-drive\0".to_vec()],
                },
            )
            .await
            .unwrap();
    });
    let request = UDisksRequest::with_timeout(Duration::from_millis(500));
    assert_eq!(
        backend.perform_validated_with_request(
            &before.devices()[0],
            Some(before.owner()),
            VolumeAction::Unmount,
            None,
            &request,
        ),
        Err(UDisksError::StaleObject)
    );
}

fn wait_for_trigger(subscription: &VolumeSubscription, expected: VolumeTrigger) {
    futures_lite::future::block_on(async {
        let deadline = async {
            async_io::Timer::after(Duration::from_secs(2)).await;
            panic!("timed out waiting for {expected:?}");
        };
        futures_lite::future::race(
            async {
                loop {
                    if subscription.recv().await.unwrap() == expected {
                        return;
                    }
                }
            },
            deadline,
        )
        .await
    });
}

#[test]
fn injected_listener_delivers_object_property_owner_signals_and_shuts_down() {
    let bus = PrivateBus::start();
    let calls = Arc::new(AtomicUsize::new(0));
    let slow = Arc::new(AtomicBool::new(false));
    let error_mode = Arc::new(AtomicUsize::new(0));
    let actions = Arc::new(Mutex::new(Vec::new()));
    let service = start_service(&bus.address, calls, Arc::clone(&slow), error_mode, actions);
    let subscription =
        VolumeSubscription::with_udisks(UDisksBusConfig::address(bus.address.as_str()), false);
    wait_for_trigger(&subscription, VolumeTrigger::MountTableChanged);
    // Registration itself reconciles the already-running service, closing the
    // signal-before-match-rule race.
    wait_for_trigger(&subscription, VolumeTrigger::UDisksChanged);

    futures_lite::future::block_on(async {
        service
            .object_server()
            .at(
                ADDED_PATH,
                FakeBlock {
                    slow: Arc::clone(&slow),
                },
            )
            .await
            .unwrap();
    });
    wait_for_trigger(&subscription, VolumeTrigger::UDisksChanged);

    futures_lite::future::block_on(async {
        let interface = service
            .object_server()
            .interface::<_, FakeBlock>(BLOCK_PATH)
            .await
            .unwrap();
        interface
            .get()
            .await
            .id_label_changed(interface.signal_emitter())
            .await
            .unwrap();
    });
    wait_for_trigger(&subscription, VolumeTrigger::UDisksChanged);

    futures_lite::future::block_on(async {
        service
            .object_server()
            .remove::<FakeBlock, _>(ADDED_PATH)
            .await
            .unwrap();
    });
    wait_for_trigger(&subscription, VolumeTrigger::UDisksChanged);

    let backend =
        ZbusUDisksBackend::connect_address_with_timeout(&bus.address, Duration::from_secs(5))
            .unwrap();
    slow.store(true, Ordering::SeqCst);
    let runtime = VolumeRuntime::from_service_with_subscription(
        VolumeService::new(
            Arc::new(backend),
            Arc::new(EmptyMounts),
            Arc::new(NoOperationUsage),
        ),
        VolumeSubscription::with_udisks(UDisksBusConfig::address(bus.address.as_str()), false),
    );
    runtime.refresh(VolumeTrigger::UDisksChanged).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while SLOW_PROPERTIES_ENTERED.load(Ordering::Acquire) == 0
        && std::time::Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    assert!(SLOW_PROPERTIES_ENTERED.load(Ordering::Acquire) > 0);
    let runtime_shutdown = std::time::Instant::now();
    drop(runtime);
    assert!(runtime_shutdown.elapsed() < Duration::from_millis(500));
    slow.store(false, Ordering::SeqCst);

    futures_lite::future::block_on(service.release_name(SERVICE)).unwrap();
    wait_for_trigger(&subscription, VolumeTrigger::ServiceOwnerChanged);
    let started = std::time::Instant::now();
    drop(subscription);
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[test]
fn listener_drop_cancels_stalled_bus_setup_without_leaking_threads() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("stalled-bus.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (release, released) = mpsc::sync_channel(1);
    let (ready, accepted) = mpsc::sync_channel(1);
    let server = std::thread::spawn(move || {
        let mut connections = Vec::new();
        let started = std::time::Instant::now();
        while connections.len() < 2 && started.elapsed() < Duration::from_secs(1) {
            match listener.accept() {
                Ok((stream, _)) => connections.push(stream),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("stalled bus accept failed: {error}"),
            }
        }
        assert_eq!(
            connections.len(),
            2,
            "both listener setups reached authentication"
        );
        ready.send(()).unwrap();
        let _ = released.recv_timeout(Duration::from_secs(2));
        drop(connections);
    });
    let config = UDisksBusConfig::address(format!("unix:path={}", socket.display()));
    let subscription = VolumeSubscription::with_udisks(config, false);
    wait_for_trigger(&subscription, VolumeTrigger::MountTableChanged);
    accepted.recv_timeout(Duration::from_secs(1)).unwrap();

    let started = std::time::Instant::now();
    drop(subscription);
    let elapsed = started.elapsed();
    let _ = release.send(());
    server.join().unwrap();
    assert!(
        elapsed < Duration::from_millis(500),
        "listener shutdown waited {elapsed:?} for stalled authentication"
    );
}

#[test]
fn listener_disconnect_reconnect_loop_remains_cancellable() {
    let mut bus = PrivateBus::start();
    let subscription =
        VolumeSubscription::with_udisks(UDisksBusConfig::address(bus.address.as_str()), false);
    wait_for_trigger(&subscription, VolumeTrigger::MountTableChanged);
    wait_for_trigger(&subscription, VolumeTrigger::UDisksChanged);
    bus.child.kill().unwrap();
    let _ = bus.child.wait();
    wait_for_trigger(&subscription, VolumeTrigger::ServiceOwnerChanged);

    let started = std::time::Instant::now();
    drop(subscription);
    assert!(started.elapsed() < Duration::from_millis(500));
}
