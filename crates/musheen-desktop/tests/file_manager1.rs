#![cfg(unix)]

use musheen_desktop::{FileManager1, FileManagerError, FileManagerRequest, FileManagerRequestSink};
use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct RecordingSink(Mutex<Vec<FileManagerRequest>>);

impl FileManagerRequestSink for RecordingSink {
    fn submit(&self, request: FileManagerRequest) -> Result<(), FileManagerError> {
        self.0.lock().unwrap().push(request);
        Ok(())
    }
}

#[test]
fn routes_standard_methods_to_one_existing_window_sink() {
    let sink = Arc::new(RecordingSink::default());
    let service = FileManager1::new(sink.clone());

    service
        .show_folders(&["file:///tmp/folder"], "startup-a")
        .unwrap();
    service
        .show_items(&["file:///tmp/folder/item.txt"], "startup-b")
        .unwrap();
    service
        .show_item_properties(&["file:///tmp/folder/item.txt"], "startup-c")
        .unwrap();

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
        assert!(service.show_items(&[uri], "startup").is_err(), "{uri}");
    }
    assert!(sink.0.lock().unwrap().is_empty());
}

#[test]
fn bounds_empty_and_oversized_requests() {
    let service = FileManager1::new(Arc::new(RecordingSink::default()));
    assert!(service.show_items(&[], "startup").is_err());
    let uris = vec!["file:///tmp/item"; 257];
    assert!(service.show_items(&uris, "startup").is_err());
}

#[test]
fn bounded_ui_channel_reports_backpressure_without_blocking() {
    let (sender, _receiver) = async_channel::bounded(1);
    let service = FileManager1::new(Arc::new(sender));
    service
        .show_items(&["file:///tmp/first"], "startup")
        .unwrap();
    assert_eq!(
        service.show_items(&["file:///tmp/second"], "startup"),
        Err(FileManagerError::Busy)
    );
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
                &(vec!["file:///tmp/folder"], "startup-private-bus"),
            )
            .await
            .unwrap();
        proxy
            .call_method(
                "ShowItems",
                &(vec!["file:///tmp/folder/item"], "startup-item"),
            )
            .await
            .unwrap();
        proxy
            .call_method(
                "ShowItemProperties",
                &(vec!["file:///tmp/folder/item"], "startup-properties"),
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
                &(vec!["file:///tmp/restarted"], "startup-restarted"),
            )
            .await
            .unwrap();
        assert_eq!(restarted_sink.0.lock().unwrap().len(), 1);
    });
    assert_eq!(sink.0.lock().unwrap().len(), 3);
}
