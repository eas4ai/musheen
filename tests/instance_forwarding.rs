#![cfg(unix)]

use musheen_core::BoxFuture;
use musheen_desktop::{
    FileManagerError, FileManagerRequest, FileManagerRequestSink, MUSHEEN_FILE_MANAGER_NAME,
};
use std::fs::File;
use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct RecordingSink(Mutex<Vec<FileManagerRequest>>);

impl FileManagerRequestSink for RecordingSink {
    fn submit(
        &self,
        request: FileManagerRequest,
    ) -> BoxFuture<'static, Result<(), FileManagerError>> {
        self.0.lock().unwrap().push(request);
        Box::pin(async { Ok(()) })
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
            .expect("instance forwarding tests require dbus-daemon");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(!address.trim().is_empty());
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

#[test]
fn second_process_forwards_its_folder_to_the_primary_instance() {
    let bus = PrivateBus::start();
    let temporary = tempfile::tempdir().unwrap();
    let runtime = temporary.path().join("runtime");
    let lock_directory = runtime.join("musheen");
    std::fs::create_dir_all(&lock_directory).unwrap();
    let lock = File::from(
        rustix::fs::open(
            lock_directory.join("instance.lock"),
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .unwrap(),
    );
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
    let folder = temporary.path().join("requested folder");
    std::fs::create_dir(&folder).unwrap();
    let sink = Arc::new(RecordingSink::default());
    let _service = futures_lite::future::block_on(musheen_desktop::serve_file_manager1_named(
        Some(&bus.address),
        MUSHEEN_FILE_MANAGER_NAME,
        sink.clone(),
    ))
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_musheen"))
        .arg(&folder)
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("XDG_CONFIG_HOME", temporary.path().join("config"))
        .env("HOME", temporary.path())
        .env_remove("DESKTOP_STARTUP_ID")
        .env_remove("XDG_ACTIVATION_TOKEN")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "second launch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = sink.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(matches!(
        requests[0],
        FileManagerRequest::ShowFolders { .. }
    ));
    assert_eq!(
        requests[0].locations()[0].as_unix_path(),
        Some(folder.as_path())
    );
}
