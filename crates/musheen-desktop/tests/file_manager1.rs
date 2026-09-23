#![cfg(unix)]

use musheen_core::BoxFuture;
use musheen_desktop::{FileManager1, FileManagerError, FileManagerRequest, FileManagerRequestSink};
use std::io::{BufRead as _, BufReader};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
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

#[test]
fn routes_standard_methods_to_one_existing_window_sink() {
    let temporary = tempfile::tempdir().unwrap();
    let folder = temporary.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    let item = folder.join("item.txt");
    std::fs::write(&item, b"fixture").unwrap();
    let folder_uri = format!("file://{}", folder.display());
    let item_uri = format!("file://{}", item.display());
    let sink = Arc::new(RecordingSink::default());
    let service = FileManager1::new(sink.clone());

    futures_lite::future::block_on(async {
        service
            .show_folders(&[&folder_uri], "startup-a")
            .await
            .unwrap();
        service.show_items(&[&item_uri], "startup-b").await.unwrap();
        service
            .show_item_properties(&[&item_uri], "startup-c")
            .await
            .unwrap();
    });

    let requests = sink.0.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].startup_id(), "startup-a");
    assert!(matches!(
        requests[0],
        FileManagerRequest::ShowFolders { .. }
    ));
    assert!(matches!(requests[1], FileManagerRequest::ShowItems { .. }));
    assert!(matches!(
        requests[2],
        FileManagerRequest::ShowItemProperties { .. }
    ));
}

#[test]
fn rejects_malformed_remote_and_lossy_file_uris_without_dispatch() {
    let sink = Arc::new(RecordingSink::default());
    let service = FileManager1::new(sink.clone());

    for uri in [
        "https://example.test/file",
        "file://other-host/tmp/file",
        "file:///tmp/%GG",
        "file:///tmp/%00name",
    ] {
        assert!(
            futures_lite::future::block_on(service.show_items(&[uri], "startup")).is_err(),
            "{uri}"
        );
    }
    assert!(sink.0.lock().unwrap().is_empty());
}

#[test]
fn percent_encoded_non_utf8_file_uri_keeps_exact_path_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let mut bytes = temporary.path().as_os_str().as_bytes().to_vec();
    bytes.extend_from_slice(b"/name-");
    bytes.push(0xff);
    let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes.clone()));
    std::fs::write(&path, b"fixture").unwrap();
    let uri = format!(
        "file://{}",
        bytes
            .iter()
            .map(|byte| match *byte {
                b'/' => "/".to_owned(),
                b'-' | b'.' | b'_' | b'~' | b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => {
                    char::from(*byte).to_string()
                }
                byte => format!("%{byte:02X}"),
            })
            .collect::<String>()
    );
    let sink = Arc::new(RecordingSink::default());
    let service = FileManager1::new(sink.clone());

    futures_lite::future::block_on(service.show_items(&[&uri], "non-utf8")).unwrap();

    let requests = sink.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].locations()[0]
            .as_unix_path()
            .unwrap()
            .as_os_str()
            .as_bytes(),
        bytes
    );
}

#[test]
fn bounds_empty_and_oversized_requests() {
    let service = FileManager1::new(Arc::new(RecordingSink::default()));
    assert!(futures_lite::future::block_on(service.show_items(&[], "startup")).is_err());
    let uris = vec!["file:///tmp/item"; 257];
    assert!(futures_lite::future::block_on(service.show_items(&uris, "startup")).is_err());
}

#[test]
fn bounded_ui_channel_reports_backpressure_without_blocking() {
    let temporary = tempfile::NamedTempFile::new().unwrap();
    let uri = format!("file://{}", temporary.path().display());
    let (sender, _receiver) = async_channel::bounded(1);
    let service = FileManager1::new(Arc::new(sender));
    let first = futures_lite::future::block_on(service.show_items(&[&uri], "startup"));
    assert_eq!(first, Err(FileManagerError::TimedOut));
    let second = futures_lite::future::block_on(service.show_items(&[&uri], "startup-again"));
    assert_eq!(second, Err(FileManagerError::Busy));
}

#[test]
fn rejects_missing_local_paths_before_routing() {
    let sink = Arc::new(RecordingSink::default());
    let service = FileManager1::new(sink.clone());
    let result = futures_lite::future::block_on(
        service.show_items(&["file:///definitely/missing/musheen-fixture"], "startup"),
    );
    assert_eq!(result, Err(FileManagerError::Unreachable));
    assert!(sink.0.lock().unwrap().is_empty());
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
            .expect("FileManager1 tests require dbus-daemon");
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
fn exports_all_standard_methods_introspection_and_recovers_after_restart() {
    let bus = PrivateBus::start();
    let temporary = tempfile::tempdir().unwrap();
    let folder = temporary.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    let item = folder.join("item");
    std::fs::write(&item, b"fixture").unwrap();
    let folder_uri = format!("file://{}", folder.display());
    let item_uri = format!("file://{}", item.display());
    let sink = Arc::new(RecordingSink::default());
    futures_lite::future::block_on(async {
        let service = musheen_desktop::serve_file_manager1(Some(&bus.address), sink.clone())
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap();
        let proxy = zbus::Proxy::new(
            &client,
            musheen_desktop::FILE_MANAGER_NAME,
            musheen_desktop::FILE_MANAGER_PATH,
            musheen_desktop::FILE_MANAGER_NAME,
        )
        .await
        .unwrap();
        proxy
            .call_method(
                "ShowFolders",
                &(vec![folder_uri.as_str()], "startup-private-bus"),
            )
            .await
            .unwrap();
        proxy
            .call_method("ShowItems", &(vec![item_uri.as_str()], "startup-item"))
            .await
            .unwrap();
        proxy
            .call_method(
                "ShowItemProperties",
                &(vec![item_uri.as_str()], "startup-properties"),
            )
            .await
            .unwrap();
        let malformed = proxy
            .call_method("ShowItems", &(vec!["https://example.test/item"], "startup"))
            .await;
        assert!(malformed.is_err());
        let introspection = zbus::fdo::IntrospectableProxy::builder(&client)
            .destination(musheen_desktop::FILE_MANAGER_NAME)
            .unwrap()
            .path(musheen_desktop::FILE_MANAGER_PATH)
            .unwrap()
            .build()
            .await
            .unwrap()
            .introspect()
            .await
            .unwrap();
        for method in ["ShowItems", "ShowFolders", "ShowItemProperties"] {
            assert!(introspection.contains(&format!("method name=\"{method}\"")));
        }
        service.close().await.unwrap();

        let restarted_sink = Arc::new(RecordingSink::default());
        let _restarted =
            musheen_desktop::serve_file_manager1(Some(&bus.address), restarted_sink.clone())
                .await
                .unwrap();
        proxy
            .call_method(
                "ShowFolders",
                &(vec![folder_uri.as_str()], "startup-restarted"),
            )
            .await
            .unwrap();
        assert_eq!(restarted_sink.0.lock().unwrap().len(), 1);
    });
    assert_eq!(sink.0.lock().unwrap().len(), 3);
}

#[test]
fn musheen_owned_service_accepts_a_forwarded_second_launch() {
    let bus = PrivateBus::start();
    let temporary = tempfile::tempdir().unwrap();
    let folder = temporary.path().join("folder with spaces");
    std::fs::create_dir(&folder).unwrap();
    let sink = Arc::new(RecordingSink::default());

    futures_lite::future::block_on(async {
        let _service = musheen_desktop::serve_file_manager1_named(
            Some(&bus.address),
            musheen_desktop::MUSHEEN_FILE_MANAGER_NAME,
            sink.clone(),
        )
        .await
        .unwrap();

        musheen_desktop::forward_show_folders_to_musheen(
            Some(&bus.address),
            &[musheen_core::StorePath::from_unix_path(folder.as_os_str())],
            "second-launch",
        )
        .await
        .unwrap();
    });

    let requests = sink.0.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].startup_id(), "second-launch");
    assert_eq!(
        requests[0].locations()[0].as_unix_path(),
        Some(folder.as_path())
    );
}
