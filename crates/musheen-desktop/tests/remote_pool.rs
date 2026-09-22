use futures_lite::future::block_on;
use futures_lite::future::poll_once;
use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::remote::{
    CONNECT_TIMEOUT, ConnectionProbe, ConnectionProfile, ConnectionProfiles, HostKeyPolicy,
    PoolLimits, PoolRuntime, ProfileConnectionTest, ProfileConnectionTester, ProviderPool,
    ProxyKind, ProxySettings, RemoteConnector, RemoteError, RemoteErrorCategory, RemoteHost,
    RemoteProtocol, SaveConfirmation, SaveRequirement, SecurityPolicy, TLS_PIN_BYTES, TestReport,
    TlsPolicy,
};
use musheen_desktop::{ConnectionId, CredentialReference};
use std::collections::VecDeque;
use std::fmt;
use std::future::pending;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::task::Waker;
use std::time::Duration;

fn credential(id: &str) -> CredentialReference {
    CredentialReference::persistent(ConnectionId::new(id).unwrap())
}

fn profile(protocol: RemoteProtocol, security: SecurityPolicy) -> ConnectionProfile {
    ConnectionProfile::new(
        ConnectionId::new(format!("{protocol:?}").to_ascii_lowercase()).unwrap(),
        format!("{protocol:?}"),
        protocol,
        RemoteHost::new(protocol, "files.example.test").unwrap(),
        None,
        "/share",
        Some("alice"),
        Some(credential("files-example")),
        security,
        None,
    )
    .unwrap()
}

#[test]
fn every_protocol_validates_and_profiles_never_serialize_inline_secrets() {
    let tls = SecurityPolicy::Tls(TlsPolicy::SystemRoots);
    let ssh = SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts);
    let plain = SecurityPolicy::PlaintextConfirmed;
    let system = SecurityPolicy::SystemManaged;
    let profiles = vec![
        profile(RemoteProtocol::Ftp, plain.clone()),
        profile(RemoteProtocol::Ftps, tls.clone()),
        profile(RemoteProtocol::Sftp, ssh),
        profile(RemoteProtocol::WebDav, tls.clone()),
        profile(RemoteProtocol::Http, tls),
        profile(RemoteProtocol::Smb, system.clone()),
        profile(RemoteProtocol::Nfs, system),
    ];

    let encoded = ConnectionProfiles::new(profiles.clone()).export().unwrap();
    assert!(encoded.contains("secret-service:files-example"));
    assert!(!encoded.contains("password"));
    assert!(!encoded.contains("hunter2"));
    assert_eq!(
        ConnectionProfiles::import(&encoded).unwrap().profiles(),
        profiles
    );

    let wrong_policy = ConnectionProfile::new(
        ConnectionId::new("invalid-policy").unwrap(),
        "Invalid policy",
        RemoteProtocol::Sftp,
        RemoteHost::new(RemoteProtocol::Sftp, "files.example.test").unwrap(),
        None,
        "/",
        None::<&str>,
        None,
        SecurityPolicy::Tls(TlsPolicy::SystemRoots),
        None,
    );
    assert_eq!(
        wrong_policy.unwrap_err().category(),
        RemoteErrorCategory::InvalidProfile
    );
    assert!(ConnectionProfiles::import(r#"{"version":99,"profiles":[]}"#).is_err());
    assert!(
        ConnectionProfiles::import(r#"{"version":1,"profiles":[],"password":"hunter2"}"#,).is_err()
    );

    let session_only = ConnectionProfile::new(
        ConnectionId::new("session-only").unwrap(),
        "Session only",
        RemoteProtocol::Sftp,
        RemoteHost::new(RemoteProtocol::Sftp, "files.example.test").unwrap(),
        None,
        "/",
        Some("alice"),
        Some(CredentialReference::session_only(
            ConnectionId::new("ephemeral").unwrap(),
        )),
        SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
        None,
    )
    .unwrap();
    assert!(
        ConnectionProfiles::new(vec![session_only])
            .export()
            .is_err()
    );
}

#[test]
fn host_tls_host_key_proxy_and_secret_references_are_strictly_validated() {
    for invalid in [
        "https://alice:secret@example.test",
        "alice@example.test",
        "example.test/path",
        "example test",
        "example.test\nforged",
        "",
    ] {
        assert!(
            RemoteHost::new(RemoteProtocol::Http, invalid).is_err(),
            "accepted {invalid:?}"
        );
    }

    let pin = [0x5a; TLS_PIN_BYTES];
    profile(
        RemoteProtocol::Ftps,
        SecurityPolicy::Tls(TlsPolicy::PinnedSha256(pin)),
    );
    profile(
        RemoteProtocol::Sftp,
        SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(pin)),
    );

    let proxy = ProxySettings::new(
        RemoteProtocol::Sftp,
        ProxyKind::Socks5,
        RemoteHost::new(RemoteProtocol::Sftp, "proxy.example.test").unwrap(),
        1080,
        Some("proxy-user"),
        Some(credential("proxy-secret")),
    )
    .unwrap();
    assert_eq!(proxy.port(), 1080);
    let proxied = ConnectionProfile::new(
        ConnectionId::new("proxied").unwrap(),
        "Proxied",
        RemoteProtocol::Sftp,
        RemoteHost::new(RemoteProtocol::Sftp, "files.example.test").unwrap(),
        Some(22),
        "/",
        Some("alice"),
        Some(credential("remote-1")),
        SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
        Some(proxy),
    )
    .unwrap();
    let encoded = ConnectionProfiles::new(vec![proxied.clone()])
        .export()
        .unwrap();
    assert!(encoded.contains("secret-service:proxy-secret"));
    assert_eq!(
        ConnectionProfiles::import(&encoded).unwrap().profiles(),
        [proxied]
    );
    let debug = format!("{:?}", ConnectionProfiles::import(&encoded).unwrap());
    assert!(!debug.contains("alice"));
    assert!(!debug.contains("remote-1"));
    assert!(!debug.contains("proxy-secret"));
    assert!(
        ProxySettings::new(
            RemoteProtocol::Sftp,
            ProxyKind::HttpConnect,
            RemoteHost::new(RemoteProtocol::Sftp, "proxy.example.test").unwrap(),
            0,
            None::<&str>,
            None,
        )
        .is_err()
    );

    let value = credential("remote-1").to_setting_value().unwrap();
    assert_eq!(value, "secret-service:remote-1");
    assert_eq!(
        CredentialReference::from_setting_value(&value).unwrap(),
        credential("remote-1")
    );
}

#[test]
fn errors_keep_safe_context_without_transport_or_credential_text() {
    let error = RemoteError::new(
        RemoteProtocol::Sftp,
        RemoteErrorCategory::Authentication,
        Some(RemoteHost::new(RemoteProtocol::Sftp, "files.example.test").unwrap()),
    );
    let rendered = format!("{error:?} {error}");
    assert!(rendered.contains("Sftp"));
    assert!(rendered.contains("Authentication"));
    assert!(rendered.contains("files.example.test"));
    for secret in ["alice", "hunter2", "secret-service:"] {
        assert!(!rendered.contains(secret));
    }
}

#[test]
fn save_flow_requires_current_test_and_explicit_risk_confirmations() {
    let old = profile(
        RemoteProtocol::Sftp,
        SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256([7; TLS_PIN_BYTES])),
    );
    let changed = profile(
        RemoteProtocol::Sftp,
        SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
    );

    assert_eq!(
        changed.save_requirement(Some(&old), None, SaveConfirmation::default()),
        SaveRequirement::TestRequired
    );
    let failed = TestReport::failed(
        &changed,
        RemoteError::new(
            RemoteProtocol::Sftp,
            RemoteErrorCategory::Network,
            Some(changed.host().clone()),
        ),
    );
    assert_eq!(
        changed.save_requirement(Some(&old), Some(&failed), SaveConfirmation::default()),
        SaveRequirement::ConfirmFailedTestAndSecurityChange
    );
    assert_eq!(
        changed.save_requirement(
            Some(&old),
            Some(&failed),
            SaveConfirmation {
                failed_test: true,
                security_change: true,
            },
        ),
        SaveRequirement::Ready
    );
    let stale = TestReport::passed(&old);
    assert_eq!(
        changed.save_requirement(Some(&old), Some(&stale), SaveConfirmation::all()),
        SaveRequirement::TestRequired
    );
}

#[test]
fn every_security_reduction_and_protocol_switch_requires_confirmation() {
    let replacement = |protocol, security| {
        ConnectionProfile::new(
            ConnectionId::new("same-profile").unwrap(),
            "Same profile",
            protocol,
            RemoteHost::new(protocol, "files.example.test").unwrap(),
            None,
            "/",
            None::<&str>,
            None,
            security,
            None,
        )
        .unwrap()
    };
    let requires_confirmation = |previous: &ConnectionProfile, next: &ConnectionProfile| {
        next.save_requirement(
            Some(previous),
            Some(&TestReport::passed(next)),
            SaveConfirmation::default(),
        )
    };
    let pin_a = [0x11; TLS_PIN_BYTES];
    let pin_b = [0x22; TLS_PIN_BYTES];
    for (previous, next) in [
        (
            replacement(
                RemoteProtocol::Http,
                SecurityPolicy::Tls(TlsPolicy::SystemRoots),
            ),
            replacement(RemoteProtocol::Http, SecurityPolicy::PlaintextConfirmed),
        ),
        (
            replacement(
                RemoteProtocol::Http,
                SecurityPolicy::Tls(TlsPolicy::PinnedSha256(pin_a)),
            ),
            replacement(
                RemoteProtocol::Http,
                SecurityPolicy::Tls(TlsPolicy::SystemRoots),
            ),
        ),
        (
            replacement(
                RemoteProtocol::Sftp,
                SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(pin_a)),
            ),
            replacement(
                RemoteProtocol::Sftp,
                SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
            ),
        ),
        (
            replacement(
                RemoteProtocol::Http,
                SecurityPolicy::Tls(TlsPolicy::PinnedSha256(pin_a)),
            ),
            replacement(
                RemoteProtocol::Http,
                SecurityPolicy::Tls(TlsPolicy::PinnedSha256(pin_b)),
            ),
        ),
        (
            replacement(
                RemoteProtocol::Sftp,
                SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
            ),
            replacement(RemoteProtocol::Http, SecurityPolicy::PlaintextConfirmed),
        ),
        (
            replacement(RemoteProtocol::Smb, SecurityPolicy::SystemManaged),
            replacement(RemoteProtocol::Nfs, SecurityPolicy::SystemManaged),
        ),
    ] {
        assert_eq!(
            requires_confirmation(&previous, &next),
            SaveRequirement::ConfirmSecurityChange
        );
    }

    for (previous, next) in [
        (
            replacement(RemoteProtocol::Http, SecurityPolicy::PlaintextConfirmed),
            replacement(
                RemoteProtocol::Http,
                SecurityPolicy::Tls(TlsPolicy::SystemRoots),
            ),
        ),
        (
            replacement(
                RemoteProtocol::Ftps,
                SecurityPolicy::Tls(TlsPolicy::SystemRoots),
            ),
            replacement(
                RemoteProtocol::Ftps,
                SecurityPolicy::Tls(TlsPolicy::PinnedSha256(pin_a)),
            ),
        ),
        (
            replacement(
                RemoteProtocol::Sftp,
                SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
            ),
            replacement(
                RemoteProtocol::Sftp,
                SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(pin_a)),
            ),
        ),
    ] {
        assert_eq!(
            requires_confirmation(&previous, &next),
            SaveRequirement::Ready
        );
    }
}

#[test]
fn invalid_hosts_retain_the_selected_protocol() {
    let error = RemoteHost::new(RemoteProtocol::Sftp, "https://bad.example").unwrap_err();
    assert_eq!(error.protocol(), RemoteProtocol::Sftp);

    let error = ConnectionProfiles::import(
        r#"{"version":1,"profiles":[{"id":"bad","name":"Bad","protocol":"sftp","host":"bad host","port":null,"path":"/","username":null,"credential":null,"security":{"kind":"ssh","policy":{"policy":"known-hosts"}},"proxy":null}]}"#,
    )
    .unwrap_err();
    assert_eq!(error.protocol(), RemoteProtocol::Sftp);
}

#[derive(Clone, Default)]
struct RecordingProbe {
    calls: Arc<Mutex<Vec<(String, u16)>>>,
    hang: bool,
}

impl ConnectionProbe for RecordingProbe {
    fn connect<'a>(
        &'a self,
        host: &'a RemoteHost,
        port: u16,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteErrorCategory>> {
        self.calls
            .lock()
            .unwrap()
            .push((host.as_str().to_owned(), port));
        Box::pin(async move { if self.hang { pending().await } else { Ok(()) } })
    }
}

#[test]
fn production_profile_tester_uses_protocol_ports_and_cancellation() {
    let probe = RecordingProbe::default();
    let tester = ProfileConnectionTester::new(probe.clone());
    let sftp = profile(
        RemoteProtocol::Sftp,
        SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
    );
    block_on(tester.test(&sftp, CancellationToken::new())).unwrap();
    assert_eq!(
        &*probe.calls.lock().unwrap(),
        &[("files.example.test".to_owned(), 22)]
    );

    let tester = ProfileConnectionTester::new(RecordingProbe {
        hang: true,
        ..RecordingProbe::default()
    });
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        block_on(tester.test(&sftp, cancellation))
            .unwrap_err()
            .category(),
        RemoteErrorCategory::Cancelled
    );
}

#[derive(Clone, Default)]
struct ManualRuntime {
    inner: Arc<ManualRuntimeInner>,
}

#[derive(Default)]
struct ManualRuntimeInner {
    now_ms: AtomicU64,
    sleepers: Mutex<Vec<(u64, Waker)>>,
    changed: Condvar,
}

impl ManualRuntime {
    fn advance(&self, duration: Duration) {
        let delta = u64::try_from(duration.as_millis()).unwrap();
        let now = self.inner.now_ms.fetch_add(delta, Ordering::AcqRel) + delta;
        let mut sleepers = self.inner.sleepers.lock().unwrap();
        let mut wake = Vec::new();
        sleepers.retain(|(deadline, waker)| {
            if *deadline <= now {
                wake.push(waker.clone());
                false
            } else {
                true
            }
        });
        drop(sleepers);
        for waker in wake {
            waker.wake();
        }
    }

    fn wait_for_sleeper(&self) {
        let mut sleepers = self.inner.sleepers.lock().unwrap();
        while sleepers.is_empty() {
            sleepers = self.inner.changed.wait(sleepers).unwrap();
        }
    }
}

impl PoolRuntime for ManualRuntime {
    fn now(&self) -> Duration {
        Duration::from_millis(self.inner.now_ms.load(Ordering::Acquire))
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        let deadline = self.now().saturating_add(duration);
        Box::pin(std::future::poll_fn(move |context| {
            if self.now() >= deadline {
                return std::task::Poll::Ready(());
            }
            let mut sleepers = self.inner.sleepers.lock().unwrap();
            if let Some(entry) = sleepers
                .iter_mut()
                .find(|(registered, _)| *registered == deadline.as_millis() as u64)
            {
                entry.1 = context.waker().clone();
            } else {
                sleepers.push((deadline.as_millis() as u64, context.waker().clone()));
                self.inner.changed.notify_all();
            }
            std::task::Poll::Pending
        }))
    }
}

#[derive(Clone)]
struct FakeConnector {
    state: Arc<FakeConnectorState>,
}

struct FakeConnectorState {
    connects: AtomicUsize,
    mode: Mutex<VecDeque<ConnectMode>>,
}

enum ConnectMode {
    Ready,
    Hang,
}

impl FakeConnector {
    fn ready() -> Self {
        Self {
            state: Arc::new(FakeConnectorState {
                connects: AtomicUsize::new(0),
                mode: Mutex::new(VecDeque::new()),
            }),
        }
    }

    fn with_modes(modes: impl IntoIterator<Item = ConnectMode>) -> Self {
        let this = Self::ready();
        this.state.mode.lock().unwrap().extend(modes);
        this
    }

    fn connects(&self) -> usize {
        self.state.connects.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
struct FakeConnection(usize);

impl RemoteConnector for FakeConnector {
    type Connection = FakeConnection;

    fn connect<'a>(
        &'a self,
        _profile: &'a ConnectionProfile,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Self::Connection, RemoteErrorCategory>> {
        Box::pin(async move {
            let number = self.state.connects.fetch_add(1, Ordering::AcqRel) + 1;
            let mode = self
                .state
                .mode
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(ConnectMode::Ready);
            match mode {
                ConnectMode::Ready => Ok(FakeConnection(number)),
                ConnectMode::Hang => pending().await,
            }
        })
    }
}

fn pool(
    connector: FakeConnector,
    runtime: ManualRuntime,
) -> ProviderPool<FakeConnector, ManualRuntime> {
    ProviderPool::with_runtime(
        profile(
            RemoteProtocol::Sftp,
            SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
        ),
        connector,
        runtime,
        PoolLimits::default(),
    )
}

#[test]
fn defaults_are_fifteen_second_connect_sixty_second_idle_four_by_eight() {
    let limits = PoolLimits::default();
    assert_eq!(limits.connect_timeout(), Duration::from_secs(15));
    assert_eq!(limits.connect_timeout(), CONNECT_TIMEOUT);
    assert_eq!(limits.idle_timeout(), Duration::from_secs(60));
    assert_eq!(limits.requests_per_connection(), 4);
    assert_eq!(limits.connections_per_provider(), 8);
}

#[test]
fn pool_caps_capacity_cancels_waiters_and_reconnects_discarded_connections() {
    let runtime = ManualRuntime::default();
    let connector = FakeConnector::ready();
    let pool = Arc::new(pool(connector.clone(), runtime));
    let mut leases = Vec::new();
    for _ in 0..32 {
        leases.push(block_on(pool.acquire(CancellationToken::new())).unwrap());
    }
    assert_eq!(pool.stats().connections(), 8);
    assert_eq!(pool.stats().active_requests(), 32);

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert_eq!(
        block_on(pool.acquire(cancelled)).unwrap_err().category(),
        RemoteErrorCategory::Cancelled
    );

    for lease in leases {
        lease.discard();
    }
    let lease = block_on(pool.acquire(CancellationToken::new())).unwrap();
    assert!(lease.connection().0 >= 9);
    assert_eq!(connector.connects(), 9);
}

#[test]
fn pool_waiters_are_fifo_and_cancellation_does_not_consume_the_next_wakeup() {
    let runtime = ManualRuntime::default();
    let connector = FakeConnector::ready();
    let limits = PoolLimits::new(Duration::from_secs(15), Duration::from_secs(60), 1, 1).unwrap();
    let pool = Arc::new(ProviderPool::with_runtime(
        profile(
            RemoteProtocol::Sftp,
            SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
        ),
        connector,
        runtime,
        limits,
    ));
    let held = block_on(pool.acquire(CancellationToken::new())).unwrap();
    let order = Arc::new(Mutex::new(Vec::new()));

    let first_cancel = CancellationToken::new();
    let first = {
        let pool = pool.clone();
        let order = order.clone();
        let cancellation = first_cancel.clone();
        std::thread::spawn(move || {
            if block_on(pool.acquire(cancellation)).is_ok() {
                order.lock().unwrap().push(1);
            }
        })
    };
    while pool.stats().waiting_requests() != 1 {
        std::thread::yield_now();
    }
    let second = {
        let pool = pool.clone();
        let order = order.clone();
        std::thread::spawn(move || {
            let _lease = block_on(pool.acquire(CancellationToken::new())).unwrap();
            order.lock().unwrap().push(2);
        })
    };
    while pool.stats().waiting_requests() != 2 {
        std::thread::yield_now();
    }
    first_cancel.cancel();
    first.join().unwrap();
    drop(held);
    second.join().unwrap();
    assert_eq!(&*order.lock().unwrap(), &[2]);
}

#[test]
fn dropping_queued_and_connecting_acquires_restores_fair_capacity() {
    let runtime = ManualRuntime::default();
    let connector = FakeConnector::ready();
    let limits = PoolLimits::new(Duration::from_secs(15), Duration::from_secs(60), 1, 1).unwrap();
    let bounded_pool = Arc::new(ProviderPool::with_runtime(
        profile(
            RemoteProtocol::Sftp,
            SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
        ),
        connector,
        runtime,
        limits,
    ));
    let held = block_on(bounded_pool.acquire(CancellationToken::new())).unwrap();
    let mut dropped_head = Box::pin(bounded_pool.acquire(CancellationToken::new()));
    assert!(block_on(poll_once(dropped_head.as_mut())).is_none());
    assert_eq!(bounded_pool.stats().waiting_requests(), 1);
    drop(dropped_head);
    assert_eq!(bounded_pool.stats().waiting_requests(), 0);
    drop(held);
    assert!(block_on(bounded_pool.acquire(CancellationToken::new())).is_ok());

    let runtime = ManualRuntime::default();
    let connector = FakeConnector::with_modes([ConnectMode::Hang, ConnectMode::Ready]);
    let pool = pool(connector.clone(), runtime);
    let mut dropped_connect = Box::pin(pool.acquire(CancellationToken::new()));
    assert!(block_on(poll_once(dropped_connect.as_mut())).is_none());
    assert_eq!(pool.stats().connecting(), 1);
    drop(dropped_connect);
    assert_eq!(pool.stats().connecting(), 0);
    assert_eq!(pool.stats().waiting_requests(), 0);
    let lease = block_on(pool.acquire(CancellationToken::new())).unwrap();
    assert_eq!(lease.connection().0, 2);
    assert_eq!(connector.connects(), 2);
}

#[test]
fn connect_timeout_and_idle_expiry_use_the_injected_runtime() {
    let runtime = ManualRuntime::default();
    let connector = FakeConnector::with_modes([ConnectMode::Hang, ConnectMode::Ready]);
    let pool = Arc::new(pool(connector.clone(), runtime.clone()));
    let attempt = {
        let pool = pool.clone();
        std::thread::spawn(move || block_on(pool.acquire(CancellationToken::new())))
    };
    runtime.wait_for_sleeper();
    runtime.advance(CONNECT_TIMEOUT);
    assert_eq!(
        attempt.join().unwrap().unwrap_err().category(),
        RemoteErrorCategory::Timeout
    );

    let lease = block_on(pool.acquire(CancellationToken::new())).unwrap();
    assert_eq!(lease.connection().0, 2);
    drop(lease);
    runtime.advance(Duration::from_secs(60));
    let lease = block_on(pool.acquire(CancellationToken::new())).unwrap();
    assert_eq!(lease.connection().0, 3);
    assert_eq!(connector.connects(), 3);
}

impl fmt::Debug for ManualRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManualRuntime")
            .field("now", &self.now())
            .finish_non_exhaustive()
    }
}
