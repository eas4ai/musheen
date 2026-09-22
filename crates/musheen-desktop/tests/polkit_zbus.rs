#![cfg(unix)]

mod support;

use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::{
    AuthorizationError, AuthorizationRequest, Authorizer, BrokerOperation, BrokerRequest, Clock,
    PolkitAuthorizer, PolkitConnectionFactory,
};
use std::collections::HashMap;
use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use support::UsableFileManager;
use zbus::zvariant::OwnedValue;

#[derive(Clone, Copy)]
struct ClockAt(u64);

impl Clock for ClockAt {
    fn now_unix_millis(&self) -> u64 {
        self.0
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
            .expect("private Polkit tests require dbus-daemon");
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

impl PolkitConnectionFactory for AddressConnection {
    fn connect(&self) -> BoxFuture<'_, Result<zbus::Connection, AuthorizationError>> {
        Box::pin(async move {
            zbus::connection::Builder::address(self.0.as_str())
                .map_err(|_| AuthorizationError::Unavailable)?
                .build()
                .await
                .map_err(|_| AuthorizationError::Unavailable)
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedRequest {
    action: String,
    details: HashMap<String, String>,
    flags: u32,
    cancellation_id: String,
}

struct FakeAuthority {
    response: (bool, bool),
    observed: Arc<Mutex<Vec<ObservedRequest>>>,
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl FakeAuthority {
    fn check_authorization(
        &self,
        subject: (&str, HashMap<String, OwnedValue>),
        action: &str,
        details: HashMap<String, String>,
        flags: u32,
        cancellation_id: &str,
    ) -> (bool, bool, HashMap<String, String>) {
        assert_eq!(subject.0, "unix-process");
        assert!(subject.1.contains_key("pid"));
        assert!(subject.1.contains_key("uid"));
        self.observed.lock().unwrap().push(ObservedRequest {
            action: action.to_owned(),
            details,
            flags,
            cancellation_id: cancellation_id.to_owned(),
        });
        (self.response.0, self.response.1, HashMap::new())
    }
}

fn start_authority(
    address: &str,
    response: (bool, bool),
    observed: Arc<Mutex<Vec<ObservedRequest>>>,
) -> zbus::Connection {
    futures_lite::future::block_on(async {
        zbus::connection::Builder::address(address)
            .unwrap()
            .name("org.freedesktop.PolicyKit1")
            .unwrap()
            .serve_at(
                "/org/freedesktop/PolicyKit1/Authority",
                FakeAuthority { response, observed },
            )
            .unwrap()
            .build()
            .await
            .unwrap()
    })
}

fn authorization_request(path: &std::path::Path) -> AuthorizationRequest {
    let request = BrokerRequest::open_directory(path).unwrap();
    AuthorizationRequest::from_broker_request(&request, musheen_desktop::PrivilegeProvider::Polkit)
}

#[test]
fn private_bus_authorization_sends_narrow_action_target_and_subject() {
    let bus = PrivateBus::start();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let _authority = start_authority(&bus.address, (true, false), Arc::clone(&observed));
    let root = tempfile::tempdir().unwrap();
    let authorizer =
        PolkitAuthorizer::with_connection(AddressConnection(bus.address.clone()), ClockAt(100))
            .with_timeout(Duration::from_secs(1));

    let grant = authorizer
        .authorize(&authorization_request(root.path()))
        .unwrap();

    assert!(grant.expires_at_unix_millis() > 100);
    let calls = observed.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].action,
        BrokerOperation::OpenDirectory {
            target: root.path().to_path_buf()
        }
        .action_id()
    );
    assert_eq!(calls[0].details["command"], calls[0].action);
    assert_eq!(calls[0].details["target"], root.path().to_str().unwrap());
    assert_eq!(calls[0].details["request-digest"].len(), 64);
    assert_eq!(calls[0].flags, 1);
    assert!(!calls[0].cancellation_id.is_empty());
}

#[test]
fn broker_side_revalidation_disables_a_second_interactive_prompt() {
    let bus = PrivateBus::start();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let _authority = start_authority(&bus.address, (true, false), Arc::clone(&observed));
    let root = tempfile::tempdir().unwrap();
    let authorizer =
        PolkitAuthorizer::with_connection(AddressConnection(bus.address.clone()), ClockAt(100))
            .with_user_interaction(false)
            .with_timeout(Duration::from_secs(1));

    authorizer
        .authorize(&authorization_request(root.path()))
        .unwrap();

    assert_eq!(observed.lock().unwrap()[0].flags, 0);
}

#[test]
fn private_bus_denial_challenge_and_absent_service_are_typed() {
    for (response, expected) in [
        ((false, false), AuthorizationError::Denied),
        ((false, true), AuthorizationError::Cancelled),
    ] {
        let bus = PrivateBus::start();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let _authority = start_authority(&bus.address, response, observed);
        let root = tempfile::tempdir().unwrap();
        let authorizer =
            PolkitAuthorizer::with_connection(AddressConnection(bus.address.clone()), ClockAt(100))
                .with_timeout(Duration::from_secs(1));
        assert_eq!(
            authorizer.authorize(&authorization_request(root.path())),
            Err(expected)
        );
    }

    let bus = PrivateBus::start();
    let root = tempfile::tempdir().unwrap();
    let absent =
        PolkitAuthorizer::with_connection(AddressConnection(bus.address.clone()), ClockAt(100))
            .with_timeout(Duration::from_millis(100));
    assert_eq!(
        absent.authorize(&authorization_request(root.path())),
        Err(AuthorizationError::Unavailable)
    );
}

#[test]
fn production_polkit_recovers_after_owner_disconnect_and_restart() {
    let bus = PrivateBus::start();
    let root = tempfile::tempdir().unwrap();
    let request = authorization_request(root.path());
    let authorizer =
        PolkitAuthorizer::with_connection(AddressConnection(bus.address.clone()), ClockAt(100))
            .with_timeout(Duration::from_millis(250));
    let first_observed = Arc::new(Mutex::new(Vec::new()));
    let first = start_authority(&bus.address, (true, false), Arc::clone(&first_observed));

    authorizer.authorize(&request).unwrap();
    assert_eq!(first_observed.lock().unwrap().len(), 1);
    drop(first);
    assert_eq!(
        authorizer.authorize(&request),
        Err(AuthorizationError::Unavailable)
    );

    let restarted_observed = Arc::new(Mutex::new(Vec::new()));
    let _restarted = start_authority(&bus.address, (true, false), Arc::clone(&restarted_observed));
    authorizer.authorize(&request).unwrap();
    assert_eq!(restarted_observed.lock().unwrap().len(), 1);
}

struct StalledAuthority {
    started: Mutex<Option<mpsc::SyncSender<()>>>,
    cancelled: Arc<AtomicBool>,
}

#[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl StalledAuthority {
    async fn check_authorization(
        &self,
        _subject: (&str, HashMap<String, OwnedValue>),
        _action: &str,
        _details: HashMap<String, String>,
        _flags: u32,
        _cancellation_id: &str,
    ) -> (bool, bool, HashMap<String, String>) {
        if let Some(started) = self.started.lock().unwrap().take() {
            let _ = started.send(());
        }
        async_io::Timer::after(Duration::from_secs(10)).await;
        (true, false, HashMap::new())
    }

    fn cancel_check_authorization(&self, _cancellation_id: &str) {
        self.cancelled.store(true, Ordering::Release);
    }
}

#[test]
fn production_polkit_cancellation_interrupts_a_pending_prompt_and_sends_cancel() {
    let bus = PrivateBus::start();
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    let _authority = futures_lite::future::block_on(async {
        zbus::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name("org.freedesktop.PolicyKit1")
            .unwrap()
            .serve_at(
                "/org/freedesktop/PolicyKit1/Authority",
                StalledAuthority {
                    started: Mutex::new(Some(started_tx)),
                    cancelled: Arc::clone(&cancelled),
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap()
    });
    let root = tempfile::tempdir().unwrap();
    let request = authorization_request(root.path());
    let token = CancellationToken::new();
    let worker_token = token.clone();
    let address = bus.address.clone();
    let worker = std::thread::spawn(move || {
        let authorizer =
            PolkitAuthorizer::with_connection(AddressConnection(address), ClockAt(100))
                .with_timeout(Duration::from_secs(5));
        authorizer.authorize_cancellable(&request, &worker_token, Duration::from_secs(5))
    });

    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("the private authority received the pending authorization");
    let file_manager = UsableFileManager::new();
    file_manager.show("polkit-slow");
    assert_eq!(file_manager.calls(), 1);
    token.cancel();
    assert_eq!(worker.join().unwrap(), Err(AuthorizationError::Cancelled));
    assert!(cancelled.load(Ordering::Acquire));
}
