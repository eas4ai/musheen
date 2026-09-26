mod support;

use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::{
    ConnectionId, CredentialReference, CredentialVault, MutationDispatch, SecretBuffer,
    SecretError, SecretPersistence, SecretServiceBackend, SecretServiceState, SecretStorage,
};
use std::collections::BTreeMap;
use std::fs;
use std::sync::{Arc, Mutex};
use support::UsableFileManager;

type StoredSecrets = BTreeMap<CredentialReference, (String, Vec<u8>)>;

#[derive(Clone)]
struct FakeSecretService {
    state: Arc<Mutex<SecretServiceState>>,
    items: Arc<Mutex<StoredSecrets>>,
    started: async_channel::Sender<()>,
    started_rx: async_channel::Receiver<()>,
    release: async_channel::Receiver<()>,
    release_tx: async_channel::Sender<()>,
    block_before_next: Arc<Mutex<bool>>,
    block_next: Arc<Mutex<bool>>,
}

impl FakeSecretService {
    fn new(state: SecretServiceState) -> Self {
        let (started, started_rx) = async_channel::bounded(1);
        let (release_tx, release) = async_channel::bounded(1);
        Self {
            state: Arc::new(Mutex::new(state)),
            items: Arc::default(),
            started,
            started_rx,
            release,
            release_tx,
            block_before_next: Arc::new(Mutex::new(false)),
            block_next: Arc::new(Mutex::new(false)),
        }
    }

    fn set_state(&self, state: SecretServiceState) {
        *self.state.lock().unwrap() = state;
    }

    fn block_next_operation(&self) {
        *self.block_next.lock().unwrap() = true;
    }

    fn block_before_next_operation(&self) {
        *self.block_before_next.lock().unwrap() = true;
    }

    async fn wait_before_if_blocked(&self) {
        let blocked = std::mem::take(&mut *self.block_before_next.lock().unwrap());
        if blocked {
            self.started.send(()).await.unwrap();
            self.release.recv().await.unwrap();
        }
    }

    async fn wait_if_blocked(&self) {
        let blocked = std::mem::take(&mut *self.block_next.lock().unwrap());
        if blocked {
            self.started.send(()).await.unwrap();
            self.release.recv().await.unwrap();
        }
    }

    fn check_state(&self) -> Result<(), SecretError> {
        match *self.state.lock().unwrap() {
            SecretServiceState::Available => Ok(()),
            SecretServiceState::Locked => Err(SecretError::Locked),
            SecretServiceState::Unavailable => Err(SecretError::Unavailable),
        }
    }
}

impl SecretServiceBackend for FakeSecretService {
    fn state(&self) -> BoxFuture<'_, Result<SecretServiceState, SecretError>> {
        Box::pin(async move { Ok(*self.state.lock().unwrap()) })
    }

    fn create<'a>(
        &'a self,
        reference: &'a CredentialReference,
        label: &'a str,
        secret: &'a SecretBuffer,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        Box::pin(async move {
            self.wait_before_if_blocked().await;
            self.check_state()?;
            dispatch.mark_dispatched();
            let bytes = secret.expose_secret(<[u8]>::to_vec);
            self.items
                .lock()
                .unwrap()
                .insert(reference.clone(), (label.to_owned(), bytes));
            self.wait_if_blocked().await;
            Ok(())
        })
    }

    fn read<'a>(
        &'a self,
        reference: &'a CredentialReference,
    ) -> BoxFuture<'a, Result<SecretBuffer, SecretError>> {
        Box::pin(async move {
            self.wait_if_blocked().await;
            self.check_state()?;
            self.items
                .lock()
                .unwrap()
                .get(reference)
                .map(|(_, secret)| SecretBuffer::new(secret.clone()))
                .ok_or(SecretError::NotFound)
        })
    }

    fn update<'a>(
        &'a self,
        reference: &'a CredentialReference,
        label: &'a str,
        secret: &'a SecretBuffer,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        self.create(reference, label, secret, dispatch)
    }

    fn delete<'a>(
        &'a self,
        reference: &'a CredentialReference,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        Box::pin(async move {
            self.check_state()?;
            dispatch.mark_dispatched();
            self.items.lock().unwrap().remove(reference);
            Ok(())
        })
    }

    fn rename<'a>(
        &'a self,
        reference: &'a CredentialReference,
        label: &'a str,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        Box::pin(async move {
            self.check_state()?;
            dispatch.mark_dispatched();
            let mut items = self.items.lock().unwrap();
            let (saved_label, _) = items.get_mut(reference).ok_or(SecretError::NotFound)?;
            *saved_label = label.to_owned();
            Ok(())
        })
    }
}

fn secret(value: &str) -> SecretBuffer {
    SecretBuffer::new(value.as_bytes().to_vec())
}

fn read_text(value: &SecretBuffer) -> String {
    value.expose_secret(|bytes| String::from_utf8(bytes.to_vec()).unwrap())
}

#[test]
fn persistent_credentials_round_trip_update_rename_and_delete_by_stable_connection_id() {
    let backend = FakeSecretService::new(SecretServiceState::Available);
    let vault = CredentialVault::new(backend.clone());
    let connection = ConnectionId::new("server-7").unwrap();
    let cancellation = CancellationToken::new();

    let reference = futures_lite::future::block_on(vault.create(
        &connection,
        "Work server",
        &secret("initial-token"),
        SecretStorage::Persistent,
        cancellation.clone(),
    ))
    .unwrap();
    assert_eq!(reference.connection_id(), &connection);
    assert_eq!(reference.persistence(), SecretPersistence::Persistent);
    assert_eq!(
        read_text(
            &futures_lite::future::block_on(vault.read(&reference, cancellation.clone())).unwrap()
        ),
        "initial-token"
    );

    futures_lite::future::block_on(vault.update(
        &reference,
        "Work server",
        &secret("replacement-token"),
        cancellation.clone(),
    ))
    .unwrap();
    futures_lite::future::block_on(vault.rename(
        &reference,
        "Renamed server",
        cancellation.clone(),
    ))
    .unwrap();
    assert_eq!(
        backend.items.lock().unwrap()[&reference].0,
        "Renamed server"
    );
    assert_eq!(
        read_text(
            &futures_lite::future::block_on(vault.read(&reference, cancellation.clone())).unwrap()
        ),
        "replacement-token"
    );

    futures_lite::future::block_on(vault.delete(&reference, cancellation.clone())).unwrap();
    assert_eq!(
        futures_lite::future::block_on(vault.read(&reference, cancellation)),
        Err(SecretError::NotFound)
    );
}

#[test]
fn locked_or_absent_service_requires_explicit_session_only_consent() {
    for state in [SecretServiceState::Locked, SecretServiceState::Unavailable] {
        let backend = FakeSecretService::new(state);
        let vault = CredentialVault::new(backend.clone());
        let connection = ConnectionId::new("offline-server").unwrap();
        let cancellation = CancellationToken::new();
        let input = secret("session-password");

        assert_eq!(
            futures_lite::future::block_on(vault.create(
                &connection,
                "Offline server",
                &input,
                SecretStorage::Persistent,
                cancellation.clone(),
            )),
            Err(SecretError::SessionOnlyAvailable(state))
        );
        assert!(backend.items.lock().unwrap().is_empty());

        let reference = futures_lite::future::block_on(vault.create(
            &connection,
            "Offline server",
            &input,
            SecretStorage::SessionOnlyConfirmed,
            cancellation.clone(),
        ))
        .unwrap();
        assert_eq!(reference.persistence(), SecretPersistence::SessionOnly);
        assert_eq!(
            read_text(
                &futures_lite::future::block_on(vault.read(&reference, cancellation.clone()))
                    .unwrap()
            ),
            "session-password"
        );
        assert!(reference.to_setting_value().is_none());

        backend.set_state(SecretServiceState::Available);
        assert!(backend.items.lock().unwrap().is_empty());
    }
}

#[test]
fn cancellation_after_mutation_dispatch_reports_indeterminate_and_never_falls_back() {
    let backend = FakeSecretService::new(SecretServiceState::Available);
    backend.block_next_operation();
    let vault = Arc::new(CredentialVault::new(backend.clone()));
    let cancellation = CancellationToken::new();
    let worker_cancel = cancellation.clone();
    let worker = std::thread::spawn({
        let vault = vault.clone();
        move || {
            futures_lite::future::block_on(vault.create(
                &ConnectionId::new("cancelled-server").unwrap(),
                "Cancelled server",
                &secret("must-not-be-saved"),
                SecretStorage::Persistent,
                worker_cancel,
            ))
        }
    });
    futures_lite::future::block_on(backend.started_rx.recv()).unwrap();
    cancellation.cancel();
    assert_eq!(worker.join().unwrap(), Err(SecretError::Indeterminate));
    assert_eq!(backend.items.lock().unwrap().len(), 1);
    drop(backend.release_tx);
}

#[test]
fn cancellation_before_mutation_dispatch_remains_cancelled() {
    let backend = FakeSecretService::new(SecretServiceState::Available);
    backend.block_before_next_operation();
    let vault = Arc::new(CredentialVault::new(backend.clone()));
    let cancellation = CancellationToken::new();
    let worker_cancel = cancellation.clone();
    let worker = std::thread::spawn({
        let vault = vault.clone();
        move || {
            futures_lite::future::block_on(vault.create(
                &ConnectionId::new("cancelled-before-dispatch").unwrap(),
                "Cancelled before dispatch",
                &secret("must-not-be-saved"),
                SecretStorage::Persistent,
                worker_cancel,
            ))
        }
    });
    futures_lite::future::block_on(backend.started_rx.recv()).unwrap();
    cancellation.cancel();
    assert_eq!(worker.join().unwrap(), Err(SecretError::Cancelled));
    assert!(backend.items.lock().unwrap().is_empty());
    drop(backend.release_tx);
}

#[test]
fn secret_values_are_redacted_and_can_be_cleared_explicitly() {
    let mut value = secret("credential-that-must-not-leak");
    let rendered = format!("{value:?}");
    assert_eq!(rendered, "SecretBuffer([REDACTED])");
    assert!(!rendered.contains("credential-that-must-not-leak"));
    value.clear();
    assert!(value.is_empty());
}

#[test]
fn workspace_selects_secret_service_async_io_and_rust_crypto() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let desktop = fs::read_to_string(root.join("crates/musheen-desktop/Cargo.toml")).unwrap();
    assert!(workspace.contains("rt-async-io-crypto-rust"));
    assert!(desktop.contains("secret-service.workspace = true"));
}

#[derive(Clone, Copy)]
enum SecretServiceMode {
    Absent,
    Slow,
    Disconnected,
    Restarted,
}

#[derive(Clone)]
struct MatrixSecretService {
    mode: Arc<Mutex<SecretServiceMode>>,
    started: async_channel::Sender<()>,
    release: async_channel::Receiver<()>,
}

fn protocol_failure<'a, T: Send + 'a>() -> BoxFuture<'a, Result<T, SecretError>> {
    Box::pin(std::future::ready(Err(SecretError::Protocol)))
}

impl SecretServiceBackend for MatrixSecretService {
    fn state(&self) -> BoxFuture<'_, Result<SecretServiceState, SecretError>> {
        let mode = *self.mode.lock().unwrap();
        let started = self.started.clone();
        let release = self.release.clone();
        Box::pin(async move {
            match mode {
                SecretServiceMode::Absent => Err(SecretError::Unavailable),
                SecretServiceMode::Disconnected => Err(SecretError::Disconnected),
                SecretServiceMode::Slow => {
                    started.send(()).await.unwrap();
                    release.recv().await.unwrap();
                    Err(SecretError::Timeout)
                }
                SecretServiceMode::Restarted => Ok(SecretServiceState::Available),
            }
        })
    }

    fn create<'a>(
        &'a self,
        _reference: &'a CredentialReference,
        _label: &'a str,
        _secret: &'a SecretBuffer,
        _dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        protocol_failure()
    }

    fn read<'a>(
        &'a self,
        _reference: &'a CredentialReference,
    ) -> BoxFuture<'a, Result<SecretBuffer, SecretError>> {
        protocol_failure()
    }

    fn update<'a>(
        &'a self,
        _reference: &'a CredentialReference,
        _label: &'a str,
        _secret: &'a SecretBuffer,
        _dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        protocol_failure()
    }

    fn delete<'a>(
        &'a self,
        _reference: &'a CredentialReference,
        _dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        protocol_failure()
    }

    fn rename<'a>(
        &'a self,
        _reference: &'a CredentialReference,
        _label: &'a str,
        _dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        protocol_failure()
    }
}

#[test]
fn secret_service_absence_slowness_disconnect_and_restart_leave_file_management_usable() {
    let (started, started_rx) = async_channel::bounded(1);
    let (release, release_rx) = async_channel::bounded(1);
    let mode = Arc::new(Mutex::new(SecretServiceMode::Absent));
    let vault = Arc::new(CredentialVault::new(MatrixSecretService {
        mode: Arc::clone(&mode),
        started,
        release: release_rx,
    }));
    let file_manager = UsableFileManager::new();

    for (service_mode, expected) in [
        (SecretServiceMode::Absent, SecretError::Unavailable),
        (SecretServiceMode::Disconnected, SecretError::Disconnected),
    ] {
        *mode.lock().unwrap() = service_mode;
        assert_eq!(
            futures_lite::future::block_on(vault.state(CancellationToken::new())),
            Err(expected)
        );
        file_manager.show("usable");
    }

    *mode.lock().unwrap() = SecretServiceMode::Slow;
    let slow_vault = Arc::clone(&vault);
    let slow = std::thread::spawn(move || {
        futures_lite::future::block_on(slow_vault.state(CancellationToken::new()))
    });
    started_rx.recv_blocking().unwrap();
    file_manager.show("still-usable");
    release.send_blocking(()).unwrap();
    assert_eq!(slow.join().unwrap(), Err(SecretError::Timeout));

    *mode.lock().unwrap() = SecretServiceMode::Restarted;
    assert_eq!(
        futures_lite::future::block_on(vault.state(CancellationToken::new())),
        Ok(SecretServiceState::Available)
    );
    file_manager.show("restarted");
    assert_eq!(file_manager.calls(), 4);
}
