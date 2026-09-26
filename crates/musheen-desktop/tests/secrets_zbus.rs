#![cfg(unix)]

use musheen_core::BoxFuture;
use musheen_desktop::{
    ConnectionId, CredentialVault, LinuxSecretService, MutationDispatch, SecretBuffer,
    SecretConnectionFactory, SecretEncryption, SecretError, SecretServiceBackend,
    SecretServiceState, SecretStorage,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Type, Value};

const SERVICE: &str = "org.freedesktop.secrets";
const ROOT: &str = "/org/freedesktop/secrets";
const COLLECTION: &str = "/org/freedesktop/secrets/collection/default";
const SESSION: &str = "/org/freedesktop/secrets/session/musheen";
const ITEM: &str = "/org/freedesktop/secrets/collection/default/item/musheen";
const PROMPT: &str = "/org/freedesktop/secrets/prompt/musheen";
const PROMPT_NONE: &str = "/";

#[derive(Clone, Debug, Default)]
struct StoredItem {
    present: bool,
    duplicate: bool,
    default_available: bool,
    label: String,
    attributes: HashMap<String, String>,
    secret: Vec<u8>,
}

#[derive(Debug, Deserialize, Serialize, Type)]
struct WireSecret {
    session: OwnedObjectPath,
    parameters: Vec<u8>,
    value: Vec<u8>,
    content_type: String,
}

#[derive(Clone)]
struct MutationGate {
    started: async_channel::Sender<()>,
    started_rx: async_channel::Receiver<()>,
    release: async_channel::Sender<()>,
    release_rx: async_channel::Receiver<()>,
}

impl MutationGate {
    fn new() -> Self {
        let (started, started_rx) = async_channel::bounded(1);
        let (release, release_rx) = async_channel::bounded(1);
        Self {
            started,
            started_rx,
            release,
            release_rx,
        }
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
            .expect("private Secret Service tests require dbus-daemon");
        let mut address = String::new();
        BufReader::new(child.stdout.take().expect("dbus-daemon stdout"))
            .read_line(&mut address)
            .expect("dbus-daemon address");
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

#[derive(Clone)]
struct AddressConnection(String);

impl SecretConnectionFactory for AddressConnection {
    fn connect(&self) -> BoxFuture<'_, Result<zbus::Connection, SecretError>> {
        Box::pin(async move {
            zbus::connection::Builder::address(self.0.as_str())
                .map_err(|_| SecretError::Unavailable)?
                .build()
                .await
                .map_err(|_| SecretError::Unavailable)
        })
    }
}

struct FakeSecretService {
    locked: bool,
    item: Arc<Mutex<StoredItem>>,
}

#[zbus::interface(name = "org.freedesktop.Secret.Service")]
impl FakeSecretService {
    fn open_session(&self, algorithm: &str, _input: Value<'_>) -> (OwnedValue, OwnedObjectPath) {
        assert_eq!(algorithm, "plain");
        (
            OwnedValue::from(zbus::zvariant::Str::from("")),
            OwnedObjectPath::try_from(SESSION).unwrap(),
        )
    }

    fn read_alias(&self, alias: &str) -> OwnedObjectPath {
        assert_eq!(alias, "default");
        let path = if self.item.lock().unwrap().default_available {
            COLLECTION
        } else {
            PROMPT_NONE
        };
        OwnedObjectPath::try_from(path).unwrap()
    }

    fn search_items(
        &self,
        attributes: HashMap<String, String>,
    ) -> (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) {
        let item = self.item.lock().unwrap();
        if !item.present || item.attributes != attributes {
            return (Vec::new(), Vec::new());
        }
        let path = OwnedObjectPath::try_from(ITEM).unwrap();
        let paths = if item.duplicate {
            vec![path.clone(), path]
        } else {
            vec![path]
        };
        if self.locked {
            (Vec::new(), paths)
        } else {
            (paths, Vec::new())
        }
    }

    #[zbus(property)]
    fn collections(&self) -> Vec<OwnedObjectPath> {
        vec![OwnedObjectPath::try_from(COLLECTION).unwrap()]
    }
}

struct FakeCollection {
    locked: bool,
    item: Arc<Mutex<StoredItem>>,
    prompt_gate: Option<MutationGate>,
}

#[zbus::interface(name = "org.freedesktop.Secret.Collection")]
impl FakeCollection {
    fn create_item(
        &self,
        mut properties: HashMap<String, OwnedValue>,
        secret: WireSecret,
        _replace: bool,
    ) -> (OwnedObjectPath, OwnedObjectPath) {
        let label = String::try_from(
            properties
                .remove("org.freedesktop.Secret.Item.Label")
                .expect("item label"),
        )
        .expect("string item label");
        let attributes = HashMap::<String, String>::try_from(
            properties
                .remove("org.freedesktop.Secret.Item.Attributes")
                .expect("item attributes"),
        )
        .expect("string item attributes");
        *self.item.lock().unwrap() = StoredItem {
            present: true,
            duplicate: false,
            default_available: true,
            label,
            attributes,
            secret: secret.value,
        };
        if self.prompt_gate.is_some() {
            (
                OwnedObjectPath::try_from(PROMPT_NONE).unwrap(),
                OwnedObjectPath::try_from(PROMPT).unwrap(),
            )
        } else {
            (
                OwnedObjectPath::try_from(ITEM).unwrap(),
                OwnedObjectPath::try_from(PROMPT_NONE).unwrap(),
            )
        }
    }

    fn search_items(&self, attributes: HashMap<String, String>) -> Vec<OwnedObjectPath> {
        let item = self.item.lock().unwrap();
        if item.present && item.attributes == attributes {
            vec![OwnedObjectPath::try_from(ITEM).unwrap()]
        } else {
            Vec::new()
        }
    }

    #[zbus(property)]
    fn locked(&self) -> bool {
        self.locked
    }

    #[zbus(property)]
    fn label(&self) -> &str {
        "Test collection"
    }

    #[zbus(property)]
    fn items(&self) -> Vec<OwnedObjectPath> {
        self.item
            .lock()
            .unwrap()
            .present
            .then(|| OwnedObjectPath::try_from(ITEM).unwrap())
            .into_iter()
            .collect()
    }

    #[zbus(property)]
    fn created(&self) -> u64 {
        0
    }

    #[zbus(property)]
    fn modified(&self) -> u64 {
        0
    }
}

struct FakePrompt {
    gate: Option<MutationGate>,
}

#[zbus::interface(name = "org.freedesktop.Secret.Prompt")]
impl FakePrompt {
    async fn prompt(&self, _window_id: &str) {
        if let Some(gate) = &self.gate {
            gate.started.send(()).await.unwrap();
            gate.release_rx.recv().await.unwrap();
        }
    }

    fn dismiss(&self) {}
}

struct FakeItem {
    item: Arc<Mutex<StoredItem>>,
    mutation_gate: Option<MutationGate>,
}

#[zbus::interface(name = "org.freedesktop.Secret.Item")]
impl FakeItem {
    fn get_secret(&self, session: OwnedObjectPath) -> WireSecret {
        WireSecret {
            session,
            parameters: Vec::new(),
            value: self.item.lock().unwrap().secret.clone(),
            content_type: "application/octet-stream".into(),
        }
    }

    async fn set_secret(&self, secret: WireSecret) {
        self.item.lock().unwrap().secret = secret.value;
        if let Some(gate) = &self.mutation_gate {
            gate.started.send(()).await.unwrap();
            gate.release_rx.recv().await.unwrap();
        }
    }

    fn delete(&self) -> OwnedObjectPath {
        self.item.lock().unwrap().present = false;
        OwnedObjectPath::try_from(PROMPT_NONE).unwrap()
    }

    #[zbus(property)]
    fn locked(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn attributes(&self) -> HashMap<String, String> {
        self.item.lock().unwrap().attributes.clone()
    }

    #[zbus(property)]
    fn label(&self) -> String {
        self.item.lock().unwrap().label.clone()
    }

    #[zbus(property)]
    fn set_label(&self, label: String) {
        self.item.lock().unwrap().label = label;
    }

    #[zbus(property)]
    fn created(&self) -> u64 {
        0
    }

    #[zbus(property)]
    fn modified(&self) -> u64 {
        0
    }
}

fn start_service(address: &str, locked: bool) -> zbus::Connection {
    start_service_with_state(address, locked, Arc::default())
}

fn start_service_with_state(
    address: &str,
    locked: bool,
    item: Arc<Mutex<StoredItem>>,
) -> zbus::Connection {
    item.lock().unwrap().default_available = true;
    start_service_fixture(address, locked, item)
}

fn start_service_fixture(
    address: &str,
    locked: bool,
    item: Arc<Mutex<StoredItem>>,
) -> zbus::Connection {
    start_service_fixture_with_gates(address, locked, item, None, None)
}

fn start_service_fixture_with_gate(
    address: &str,
    locked: bool,
    item: Arc<Mutex<StoredItem>>,
    mutation_gate: Option<MutationGate>,
) -> zbus::Connection {
    start_service_fixture_with_gates(address, locked, item, mutation_gate, None)
}

fn start_service_fixture_with_gates(
    address: &str,
    locked: bool,
    item: Arc<Mutex<StoredItem>>,
    mutation_gate: Option<MutationGate>,
    prompt_gate: Option<MutationGate>,
) -> zbus::Connection {
    futures_lite::future::block_on(async {
        zbus::connection::Builder::address(address)
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(
                ROOT,
                FakeSecretService {
                    locked,
                    item: item.clone(),
                },
            )
            .unwrap()
            .serve_at(
                COLLECTION,
                FakeCollection {
                    locked,
                    item: item.clone(),
                    prompt_gate: prompt_gate.clone(),
                },
            )
            .unwrap()
            .serve_at(
                ITEM,
                FakeItem {
                    item,
                    mutation_gate,
                },
            )
            .unwrap()
            .serve_at(PROMPT, FakePrompt { gate: prompt_gate })
            .unwrap()
            .build()
            .await
            .unwrap()
    })
}

#[test]
fn cancellation_during_secret_service_prompt_is_indeterminate_not_session_fallback() {
    let bus = PrivateBus::start();
    let item = Arc::<Mutex<StoredItem>>::default();
    item.lock().unwrap().default_available = true;
    let prompt = MutationGate::new();
    let _service = start_service_fixture_with_gates(
        &bus.address,
        false,
        item.clone(),
        None,
        Some(prompt.clone()),
    );
    let vault = Arc::new(CredentialVault::new(
        LinuxSecretService::with_connection(
            AddressConnection(bus.address.clone()),
            SecretEncryption::Plain,
        )
        .with_timeout(Duration::from_secs(2)),
    ));
    let cancellation = musheen_core::CancellationToken::new();
    let worker_cancellation = cancellation.clone();
    let worker = std::thread::spawn({
        let vault = vault.clone();
        move || {
            futures_lite::future::block_on(vault.create(
                &ConnectionId::new("prompted-server").unwrap(),
                "Prompted",
                &SecretBuffer::new(b"prompt-secret".to_vec()),
                SecretStorage::Persistent,
                worker_cancellation,
            ))
        }
    });
    futures_lite::future::block_on(prompt.started_rx.recv()).unwrap();
    cancellation.cancel();
    assert_eq!(worker.join().unwrap(), Err(SecretError::Indeterminate));
    assert!(item.lock().unwrap().present);
    futures_lite::future::block_on(prompt.release.send(())).unwrap();
}

#[test]
fn production_adapter_fails_closed_when_owner_is_replaced_after_mutation_dispatch() {
    let bus = PrivateBus::start();
    let item = Arc::<Mutex<StoredItem>>::default();
    item.lock().unwrap().default_available = true;
    let gate = MutationGate::new();
    let service =
        start_service_fixture_with_gate(&bus.address, false, item.clone(), Some(gate.clone()));
    let backend = Arc::new(LinuxSecretService::with_connection(
        AddressConnection(bus.address.clone()),
        SecretEncryption::Plain,
    ));
    let reference = musheen_desktop::CredentialReference::persistent(
        ConnectionId::new("replaced-owner").unwrap(),
    );
    let create_dispatch = MutationDispatch::new();
    futures_lite::future::block_on(backend.create(
        &reference,
        "Original",
        &SecretBuffer::new(b"first".to_vec()),
        &create_dispatch,
    ))
    .unwrap();

    let worker = std::thread::spawn({
        let backend = backend.clone();
        let reference = reference.clone();
        move || {
            let update_dispatch = MutationDispatch::new();
            futures_lite::future::block_on(backend.update(
                &reference,
                "Replacement",
                &SecretBuffer::new(b"committed-before-reply".to_vec()),
                &update_dispatch,
            ))
        }
    });
    futures_lite::future::block_on(gate.started_rx.recv()).unwrap();
    drop(service);
    let replacement_item = Arc::<Mutex<StoredItem>>::default();
    replacement_item.lock().unwrap().default_available = true;
    let _replacement = start_service_fixture(&bus.address, false, replacement_item);
    futures_lite::future::block_on(gate.release.send(())).unwrap();

    assert_eq!(worker.join().unwrap(), Err(SecretError::Indeterminate));
    assert_eq!(item.lock().unwrap().secret, b"committed-before-reply");
}

#[test]
fn production_adapter_rejects_duplicate_stale_credentials() {
    let bus = PrivateBus::start();
    let item = Arc::<Mutex<StoredItem>>::default();
    let _service = start_service_with_state(&bus.address, false, item.clone());
    let backend = LinuxSecretService::with_connection(
        AddressConnection(bus.address.clone()),
        SecretEncryption::Plain,
    )
    .with_timeout(Duration::from_secs(1));
    let vault = CredentialVault::new(backend);
    let cancellation = musheen_core::CancellationToken::new();
    let reference = futures_lite::future::block_on(vault.create(
        &ConnectionId::new("duplicate-server").unwrap(),
        "Duplicate",
        &SecretBuffer::new(b"secret".to_vec()),
        SecretStorage::Persistent,
        cancellation.clone(),
    ))
    .unwrap();
    item.lock().unwrap().duplicate = true;

    assert_eq!(
        futures_lite::future::block_on(vault.read(&reference, cancellation.clone())),
        Err(SecretError::Ambiguous)
    );
    assert_eq!(
        futures_lite::future::block_on(vault.create(
            reference.connection_id(),
            "Should not replace one",
            &SecretBuffer::new(b"replacement".to_vec()),
            SecretStorage::Persistent,
            cancellation.clone(),
        )),
        Err(SecretError::Ambiguous)
    );
    assert_eq!(
        futures_lite::future::block_on(vault.update(
            &reference,
            "Should not update one",
            &SecretBuffer::new(b"replacement".to_vec()),
            cancellation.clone(),
        )),
        Err(SecretError::Ambiguous)
    );
    assert_eq!(
        futures_lite::future::block_on(vault.rename(
            &reference,
            "Should not choose one",
            cancellation.clone(),
        )),
        Err(SecretError::Ambiguous)
    );
    assert_eq!(
        futures_lite::future::block_on(vault.delete(&reference, cancellation)),
        Err(SecretError::Ambiguous)
    );
}

#[test]
fn missing_default_collection_offers_session_only_instead_of_not_found() {
    let bus = PrivateBus::start();
    let item = Arc::<Mutex<StoredItem>>::default();
    item.lock().unwrap().default_available = false;
    let _service = start_service_fixture(&bus.address, false, item);
    let backend = LinuxSecretService::with_connection(
        AddressConnection(bus.address.clone()),
        SecretEncryption::Plain,
    )
    .with_timeout(Duration::from_secs(1));
    let vault = CredentialVault::new(backend);
    let result = futures_lite::future::block_on(vault.create(
        &ConnectionId::new("no-default").unwrap(),
        "No default collection",
        &SecretBuffer::new(b"secret".to_vec()),
        SecretStorage::Persistent,
        musheen_core::CancellationToken::new(),
    ));
    assert_eq!(
        result,
        Err(SecretError::SessionOnlyAvailable(
            SecretServiceState::Unavailable
        ))
    );
    let reference = musheen_desktop::CredentialReference::persistent(
        ConnectionId::new("missing-credential").unwrap(),
    );
    assert_eq!(
        futures_lite::future::block_on(
            vault.read(&reference, musheen_core::CancellationToken::new(),)
        ),
        Err(SecretError::NotFound)
    );
}

#[test]
fn production_adapter_round_trips_crud_and_stable_rename_over_private_bus() {
    let bus = PrivateBus::start();
    let item = Arc::<Mutex<StoredItem>>::default();
    let _service = start_service_with_state(&bus.address, false, item.clone());
    let backend = LinuxSecretService::with_connection(
        AddressConnection(bus.address.clone()),
        SecretEncryption::Plain,
    )
    .with_timeout(Duration::from_secs(1));
    let vault = CredentialVault::new(backend);
    let connection = ConnectionId::new("private-bus-server").unwrap();
    let cancellation = musheen_core::CancellationToken::new();

    let reference = futures_lite::future::block_on(vault.create(
        &connection,
        "Initial label",
        &SecretBuffer::new(b"first-secret".to_vec()),
        SecretStorage::Persistent,
        cancellation.clone(),
    ))
    .unwrap();
    let first =
        futures_lite::future::block_on(vault.read(&reference, cancellation.clone())).unwrap();
    assert_eq!(
        first.expose_secret(<[u8]>::to_vec),
        b"first-secret".to_vec()
    );

    // The Secret Service default alias may change independently of a saved
    // connection. Updating the all-collections match must not create another.
    item.lock().unwrap().default_available = false;
    futures_lite::future::block_on(vault.update(
        &reference,
        "Initial label",
        &SecretBuffer::new(b"second-secret".to_vec()),
        cancellation.clone(),
    ))
    .unwrap();
    futures_lite::future::block_on(vault.rename(&reference, "Renamed label", cancellation.clone()))
        .unwrap();
    assert_eq!(item.lock().unwrap().label, "Renamed label");
    assert_eq!(reference.connection_id(), &connection);

    futures_lite::future::block_on(vault.delete(&reference, cancellation.clone())).unwrap();
    assert_eq!(
        futures_lite::future::block_on(vault.read(&reference, cancellation)),
        Err(SecretError::NotFound)
    );
}

#[test]
fn production_adapter_reports_absence_and_reconnects_after_owner_restart() {
    let bus = PrivateBus::start();
    let backend = LinuxSecretService::with_connection(
        AddressConnection(bus.address.clone()),
        SecretEncryption::Plain,
    )
    .with_timeout(Duration::from_millis(200));

    assert_eq!(
        futures_lite::future::block_on(backend.state()).unwrap(),
        SecretServiceState::Unavailable
    );
    let first = start_service(&bus.address, false);
    assert_eq!(
        futures_lite::future::block_on(backend.state()).unwrap(),
        SecretServiceState::Available
    );
    drop(first);
    assert_eq!(
        futures_lite::future::block_on(backend.state()).unwrap(),
        SecretServiceState::Unavailable
    );
    let _replacement = start_service(&bus.address, true);
    assert_eq!(
        futures_lite::future::block_on(backend.state()).unwrap(),
        SecretServiceState::Locked
    );
}
