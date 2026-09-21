#![cfg(unix)]

use musheen_desktop::{UDisksBackend, UDisksError, VolumeAction, ZbusUDisksBackend};
use std::collections::HashMap;
use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use zbus::fdo::ObjectManager;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

const SERVICE: &str = "org.freedesktop.UDisks2";
const ROOT: &str = "/org/freedesktop/UDisks2";
const BLOCK_PATH: &str = "/org/freedesktop/UDisks2/block_devices/fake1";

struct PrivateBus {
    child: Child,
    address: String,
}

impl PrivateBus {
    fn start() -> Option<Self> {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut address = String::new();
        BufReader::new(child.stdout.take()?)
            .read_line(&mut address)
            .ok()?;
        Some(Self {
            child,
            address: address.trim().to_owned(),
        })
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

#[zbus::interface(name = "org.freedesktop.UDisks2.Block")]
impl FakeBlock {
    #[zbus(property)]
    fn device(&self) -> Vec<u8> {
        if self.slow.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_secs(2));
        }
        b"/dev/fake1\0".to_vec()
    }

    #[zbus(property)]
    fn symlinks(&self) -> Vec<Vec<u8>> {
        vec![b"/dev/disk/by-id/fake-drive\0".to_vec()]
    }

    #[zbus(property)]
    fn id_uuid(&self) -> &str {
        "cloneable-uuid"
    }

    #[zbus(property)]
    fn id_label(&self) -> &str {
        "Fake disk"
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
        OwnedObjectPath::try_from("/").unwrap()
    }
}

struct FakeFilesystem {
    unmount_calls: Arc<AtomicUsize>,
}

#[zbus::interface(name = "org.freedesktop.UDisks2.Filesystem")]
impl FakeFilesystem {
    #[zbus(property)]
    fn mount_points(&self) -> Vec<Vec<u8>> {
        vec![b"/media/fake\0".to_vec()]
    }

    fn unmount(&self, options: HashMap<String, OwnedValue>) -> zbus::fdo::Result<()> {
        assert!(
            options.is_empty(),
            "UDisks options must be explicit and empty"
        );
        self.unmount_calls.fetch_add(1, Ordering::SeqCst);
        Err(zbus::fdo::Error::Failed("DeviceBusy fixture".into()))
    }
}

fn start_service(
    address: &str,
    calls: Arc<AtomicUsize>,
    slow: Arc<AtomicBool>,
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
                FakeFilesystem {
                    unmount_calls: calls,
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap()
    })
}

#[test]
fn fake_udisks_object_manager_properties_owner_restart_and_errors() {
    let Some(mut bus) = PrivateBus::start() else {
        return;
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let slow = Arc::new(AtomicBool::new(false));
    let first_service = start_service(&bus.address, Arc::clone(&calls), Arc::clone(&slow));
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

    futures_lite::future::block_on(first_service.release_name(SERVICE)).unwrap();
    assert!(matches!(
        backend.snapshot(),
        Err(UDisksError::Unavailable(_))
    ));
    let second_service = start_service(&bus.address, Arc::clone(&calls), Arc::clone(&slow));
    let restarted = backend.snapshot().unwrap();
    assert_ne!(first.owner(), restarted.owner());

    slow.store(true, Ordering::SeqCst);
    let started = std::time::Instant::now();
    match backend.snapshot() {
        Err(UDisksError::Timeout(_)) => {}
        other => panic!("slow property must hit the production deadline, got {other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(second_service);

    bus.child.kill().unwrap();
    let _ = bus.child.wait();
    match backend.snapshot() {
        Err(UDisksError::Disconnected(_) | UDisksError::Timeout(_)) => {}
        other => panic!("dead private bus must be a connection error, got {other:?}"),
    }
}
