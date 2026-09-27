use futures_lite::future::block_on;
use musheen_core::{
    CancellationToken, CapabilityKind, CapabilityState, ItemKind, MutationRequest, PageRequest,
    ProviderId, ResourceLimits, Store, StoreError,
};
use musheen_desktop::remote::{
    SmbBackend, SmbBackendError, SmbBackendErrorKind, SmbEntry, SmbShare, SmbStore,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

#[derive(Default)]
struct BackendState {
    calls: AtomicUsize,
    reconnects: AtomicUsize,
    renames: Mutex<Vec<(String, String)>>,
    thread_names: Mutex<Vec<String>>,
    gate: (Mutex<bool>, Condvar),
}

struct RecordingBackend {
    state: Arc<BackendState>,
    block_first_call: bool,
    retry_first_call: bool,
}

impl RecordingBackend {
    fn record_thread(&self) {
        self.state
            .thread_names
            .lock()
            .expect("thread log lock")
            .push(thread::current().name().unwrap_or("unnamed").to_owned());
    }

    fn entries() -> Vec<SmbEntry> {
        vec![
            SmbEntry::new("/Readme", "Readme", ItemKind::RegularFile, Some(4), None),
            SmbEntry::new("/README", "README", ItemKind::RegularFile, Some(8), None),
        ]
    }
}

impl SmbBackend for RecordingBackend {
    fn list_directory(&self, _path: &str) -> Result<Vec<SmbEntry>, SmbBackendError> {
        self.record_thread();
        let call = self.state.calls.fetch_add(1, Ordering::SeqCst);
        if self.block_first_call && call == 0 {
            let (lock, wake) = &self.state.gate;
            let mut open = lock.lock().expect("gate lock");
            while !*open {
                open = wake.wait(open).expect("gate wait");
            }
        }
        if self.retry_first_call && call == 0 {
            return Err(SmbBackendError::new(SmbBackendErrorKind::Retryable));
        }
        Ok(Self::entries())
    }

    fn resolve(&self, path: &str) -> Result<Option<SmbEntry>, SmbBackendError> {
        self.record_thread();
        Ok(Self::entries()
            .into_iter()
            .find(|entry| entry.path() == path))
    }

    fn rename(&self, source: &str, destination: &str) -> Result<(), SmbBackendError> {
        self.record_thread();
        self.state
            .renames
            .lock()
            .expect("rename log lock")
            .push((source.to_owned(), destination.to_owned()));
        Ok(())
    }

    fn list_shares(&self) -> Result<Vec<SmbShare>, SmbBackendError> {
        self.record_thread();
        Ok(vec![SmbShare::new("Documents"), SmbShare::new("Media")])
    }

    fn reconnect(&self) -> Result<(), SmbBackendError> {
        self.record_thread();
        self.state.reconnects.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn store(backend: RecordingBackend) -> SmbStore {
    SmbStore::new(
        ProviderId::new("smb-contract").expect("provider ID"),
        Arc::new(backend),
    )
    .expect("SMB store")
}

#[test]
fn smb_calls_use_the_dedicated_worker_and_preserve_case_collisions() {
    let state = Arc::new(BackendState::default());
    let store = store(RecordingBackend {
        state: Arc::clone(&state),
        block_first_call: false,
        retry_first_call: false,
    });

    let page = block_on(store.read_directory(
        &store.root_path(),
        PageRequest::first(&ResourceLimits::default()),
        CancellationToken::new(),
    ))
    .expect("directory page");

    assert_eq!(page.items().len(), 2);
    assert_ne!(page.items()[0].id(), page.items()[1].id());
    assert!(
        state
            .thread_names
            .lock()
            .expect("thread log")
            .iter()
            .all(|name| name.starts_with("musheen-smb-"))
    );
    assert!(matches!(
        store
            .capabilities(&store.root_path())
            .get(CapabilityKind::CaseSensitivity),
        CapabilityState::Unsupported(_)
    ));
    assert!(matches!(
        store
            .capabilities(&store.root_path())
            .get(CapabilityKind::Permissions),
        CapabilityState::Unknown(_)
    ));
}

#[test]
fn cancellation_hands_control_back_while_the_native_call_finishes_safely() {
    let state = Arc::new(BackendState::default());
    let store = Arc::new(store(RecordingBackend {
        state: Arc::clone(&state),
        block_first_call: true,
        retry_first_call: false,
    }));
    let cancellation = CancellationToken::new();
    let task_store = Arc::clone(&store);
    let task_cancellation = cancellation.clone();
    let task = thread::spawn(move || {
        block_on(task_store.read_directory(
            &task_store.root_path(),
            PageRequest::first(&ResourceLimits::default()),
            task_cancellation,
        ))
    });

    while state.calls.load(Ordering::SeqCst) == 0 {
        thread::yield_now();
    }
    cancellation.cancel();
    assert_eq!(
        task.join().expect("request thread"),
        Err(StoreError::Cancelled)
    );

    let (lock, wake) = &state.gate;
    *lock.lock().expect("gate lock") = true;
    wake.notify_all();
}

#[test]
fn retryable_reads_reconnect_once_and_mutations_use_server_side_rename() {
    let state = Arc::new(BackendState::default());
    let store = store(RecordingBackend {
        state: Arc::clone(&state),
        block_first_call: false,
        retry_first_call: true,
    });
    block_on(store.read_directory(
        &store.root_path(),
        PageRequest::first(&ResourceLimits::default()),
        CancellationToken::new(),
    ))
    .expect("retry succeeds");
    assert_eq!(state.reconnects.load(Ordering::SeqCst), 1);

    let source = store.path("/Readme").expect("source path");
    let destination = store.path("/Guide").expect("destination path");
    block_on(store.mutate(
        MutationRequest::Rename {
            source,
            destination,
        },
        CancellationToken::new(),
    ))
    .expect("server-side rename");
    assert_eq!(
        state.renames.lock().expect("rename log").as_slice(),
        &[("/Readme".to_owned(), "/Guide".to_owned())]
    );
    assert_eq!(
        block_on(store.list_shares(CancellationToken::new())).expect("share list"),
        vec![SmbShare::new("Documents"), SmbShare::new("Media")]
    );
}

#[test]
fn source_tree_has_no_afp_wire_implementation_or_dependency() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    let forbidden = format!("{}{}", "a", "fp");
    for relative in ["Cargo.toml", "crates/musheen-desktop/Cargo.toml"] {
        let source =
            std::fs::read_to_string(repository.join(relative)).expect("source audit input");
        assert!(!source.to_ascii_lowercase().contains(&forbidden));
    }
    let remote = repository.join("crates/musheen-desktop/src/remote");
    for entry in std::fs::read_dir(remote).expect("remote source directory") {
        let path = entry.expect("remote source entry").path();
        if path.extension().is_some_and(|extension| extension == "rs") {
            let source = std::fs::read_to_string(path).expect("remote source audit input");
            assert!(!source.to_ascii_lowercase().contains(&forbidden));
        }
    }
}
