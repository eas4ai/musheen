use futures_lite::future::block_on;
use futures_lite::future::poll_once;
use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::remote::{
    CONNECT_TIMEOUT, ConnectionProbe, ConnectionProfile, ConnectionProfiles, CredentialResolver,
    HostKeyPolicy, PoolLimits, PoolRuntime, ProfileConnectionTest, ProfileConnectionTester,
    ProtocolConnectionProbe, ProviderPool, ProxyKind, ProxySettings, RemoteConnector, RemoteError,
    RemoteErrorCategory, RemoteHost, RemoteProtocol, SaveConfirmation, SaveRequirement,
    SecurityPolicy, TLS_PIN_BYTES, TestReport, TlsPolicy,
};
use musheen_desktop::{ConnectionId, CredentialReference, SecretBuffer};
use std::collections::VecDeque;
use std::fmt;
use std::future::pending;
use std::io::{Read, Write};
use std::net::TcpListener;
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
}

#[test]
fn profile_round_trip_preserves_path_and_username_whitespace() {
    let profile = ConnectionProfile::new(
        ConnectionId::new("significant-whitespace").unwrap(),
        "Whitespace",
        RemoteProtocol::Sftp,
        RemoteHost::new(RemoteProtocol::Sftp, "files.example.test").unwrap(),
        None,
        "/directory with trailing space ",
        Some("alice "),
        None,
        SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
        None,
    )
    .unwrap();
    let encoded = ConnectionProfiles::new(vec![profile.clone()])
        .export()
        .unwrap();
    let decoded = ConnectionProfiles::import(&encoded).unwrap();
    assert_eq!(decoded.profiles(), [profile]);
    assert_eq!(
        decoded.profiles()[0].path(),
        "/directory with trailing space "
    );
    assert_eq!(decoded.profiles()[0].username(), Some("alice "));
}

#[test]
fn session_credentials_are_rejected_at_every_profile_boundary() {
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
    .unwrap_err();
    assert_eq!(session_only.category(), RemoteErrorCategory::InvalidProfile);

    let session_proxy = ProxySettings::new(
        RemoteProtocol::Sftp,
        ProxyKind::Socks5,
        RemoteHost::new(RemoteProtocol::Sftp, "proxy.example.test").unwrap(),
        1080,
        None::<&str>,
        Some(CredentialReference::session_only(
            ConnectionId::new("proxy-session").unwrap(),
        )),
    )
    .unwrap_err();
    assert_eq!(
        session_proxy.category(),
        RemoteErrorCategory::InvalidProfile
    );

    for encoded in [
        r#"{"version":1,"profiles":[{"id":"bad-direct","name":"Bad direct","protocol":"sftp","host":"files.example.test","port":null,"path":"/","username":null,"credential":"session-only:direct","security":{"kind":"ssh","policy":{"policy":"known-hosts"}},"proxy":null}]}"#,
        r#"{"version":1,"profiles":[{"id":"bad-proxy","name":"Bad proxy","protocol":"sftp","host":"files.example.test","port":null,"path":"/","username":null,"credential":null,"security":{"kind":"ssh","policy":{"policy":"known-hosts"}},"proxy":{"kind":"socks5","host":"proxy.example.test","port":1080,"username":null,"credential":"session-only:proxy"}}]}"#,
    ] {
        let error = ConnectionProfiles::import(encoded).unwrap_err();
        assert_eq!(error.protocol(), RemoteProtocol::Sftp);
        assert_eq!(error.category(), RemoteErrorCategory::InvalidProfile);
    }
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
fn system_managed_protocols_reject_hidden_proxy_state() {
    for protocol in [RemoteProtocol::Smb, RemoteProtocol::Nfs] {
        let proxy = ProxySettings::new(
            protocol,
            ProxyKind::Socks5,
            RemoteHost::new(protocol, "proxy.example.test").unwrap(),
            1080,
            None::<&str>,
            None,
        )
        .unwrap();
        let error = ConnectionProfile::new(
            ConnectionId::new("system-managed").unwrap(),
            "System managed",
            protocol,
            RemoteHost::new(protocol, "files.example.test").unwrap(),
            None,
            "/share",
            None::<&str>,
            None,
            SecurityPolicy::SystemManaged,
            Some(proxy),
        )
        .unwrap_err();
        assert_eq!(error.protocol(), protocol);
        assert_eq!(error.category(), RemoteErrorCategory::InvalidProfile);
    }

    let encoded = r#"{"version":1,"profiles":[{"id":"bad","name":"Bad","protocol":"smb","host":"files.example.test","port":null,"path":"/share","username":null,"credential":null,"security":{"kind":"system-managed"},"proxy":{"kind":"socks5","host":"proxy.example.test","port":1080,"username":null,"credential":null}}]}"#;
    assert_eq!(
        ConnectionProfiles::import(encoded).unwrap_err().category(),
        RemoteErrorCategory::InvalidProfile
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
    calls: Arc<Mutex<Vec<ConnectionProfile>>>,
    hang: bool,
}

impl ConnectionProbe for RecordingProbe {
    fn connect<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteErrorCategory>> {
        self.calls.lock().unwrap().push(profile.clone());
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
        probe.calls.lock().unwrap().as_slice(),
        std::slice::from_ref(&sftp)
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

#[derive(Clone, Copy)]
struct StaticCredentialResolver;

impl CredentialResolver for StaticCredentialResolver {
    fn resolve<'a>(
        &'a self,
        _reference: &'a CredentialReference,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<SecretBuffer, RemoteErrorCategory>> {
        Box::pin(async { Ok(SecretBuffer::new(b"correct horse".to_vec())) })
    }
}

fn read_headers(stream: &mut std::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut byte = [0];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    String::from_utf8(bytes).unwrap()
}

#[test]
fn protocol_tester_uses_authenticated_proxy_and_checks_configured_http_path() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let connect = read_headers(&mut stream);
        assert!(connect.starts_with("CONNECT must-not-resolve.invalid:80 HTTP/1.1\r\n"));
        assert!(connect.contains("Proxy-Authorization: Basic "));
        stream.write_all(b"HTTP/1.1 200 Connected\r\n\r\n").unwrap();
        let request = read_headers(&mut stream);
        assert!(request.starts_with("HEAD /folder%20 HTTP/1.1\r\n"));
        assert!(request.contains("Authorization: Basic "));
        stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
    });
    let protocol = RemoteProtocol::Http;
    let proxy = ProxySettings::new(
        protocol,
        ProxyKind::HttpConnect,
        RemoteHost::new(protocol, "127.0.0.1").unwrap(),
        proxy_port,
        Some("proxy user"),
        Some(credential("proxy")),
    )
    .unwrap();
    let profile = ConnectionProfile::new(
        ConnectionId::new("proxy-http").unwrap(),
        "Proxy HTTP",
        protocol,
        RemoteHost::new(protocol, "must-not-resolve.invalid").unwrap(),
        None,
        "/folder ",
        Some("alice"),
        Some(credential("target")),
        SecurityPolicy::PlaintextConfirmed,
        Some(proxy),
    )
    .unwrap();
    let tester = ProfileConnectionTester::new(ProtocolConnectionProbe::new(Arc::new(
        StaticCredentialResolver,
    )));
    block_on(tester.test(&profile, CancellationToken::new())).unwrap();
    server.join().unwrap();
}

#[test]
fn protocol_tester_rejects_proxy_auth_and_unsupported_protocol_false_success() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _request = read_headers(&mut stream);
        stream
            .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
            .unwrap();
    });
    let protocol = RemoteProtocol::Http;
    let proxy = ProxySettings::new(
        protocol,
        ProxyKind::HttpConnect,
        RemoteHost::new(protocol, "127.0.0.1").unwrap(),
        proxy_port,
        Some("proxy"),
        Some(credential("proxy")),
    )
    .unwrap();
    let http = ConnectionProfile::new(
        ConnectionId::new("bad-proxy").unwrap(),
        "Bad proxy",
        protocol,
        RemoteHost::new(protocol, "must-not-resolve.invalid").unwrap(),
        None,
        "/",
        None::<&str>,
        None,
        SecurityPolicy::PlaintextConfirmed,
        Some(proxy),
    )
    .unwrap();
    let tester = ProfileConnectionTester::new(ProtocolConnectionProbe::new(Arc::new(
        StaticCredentialResolver,
    )));
    assert_eq!(
        block_on(tester.test(&http, CancellationToken::new()))
            .unwrap_err()
            .category(),
        RemoteErrorCategory::Authentication
    );
    server.join().unwrap();

    let sftp = profile(
        RemoteProtocol::Sftp,
        SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
    );
    assert_eq!(
        block_on(tester.test(&sftp, CancellationToken::new()))
            .unwrap_err()
            .category(),
        RemoteErrorCategory::Unavailable
    );
}

#[test]
fn protocol_tester_sends_target_name_through_socks_without_direct_dns() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut greeting = [0; 3];
        stream.read_exact(&mut greeting).unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        stream.write_all(&[5, 0]).unwrap();
        let mut head = [0; 5];
        stream.read_exact(&mut head).unwrap();
        assert_eq!(&head[..4], &[5, 1, 0, 3]);
        let mut host = vec![0; usize::from(head[4])];
        stream.read_exact(&mut host).unwrap();
        assert_eq!(host, b"must-not-resolve.invalid");
        let mut port = [0; 2];
        stream.read_exact(&mut port).unwrap();
        assert_eq!(u16::from_be_bytes(port), 80);
        stream.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).unwrap();
        let request = read_headers(&mut stream);
        assert!(request.starts_with("HEAD / HTTP/1.1\r\n"));
        stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
    });
    let protocol = RemoteProtocol::Http;
    let proxy = ProxySettings::new(
        protocol,
        ProxyKind::Socks5,
        RemoteHost::new(protocol, "127.0.0.1").unwrap(),
        proxy_port,
        None::<&str>,
        None,
    )
    .unwrap();
    let http = ConnectionProfile::new(
        ConnectionId::new("socks-http").unwrap(),
        "SOCKS HTTP",
        protocol,
        RemoteHost::new(protocol, "must-not-resolve.invalid").unwrap(),
        None,
        "/",
        None::<&str>,
        None,
        SecurityPolicy::PlaintextConfirmed,
        Some(proxy),
    )
    .unwrap();
    let tester = ProfileConnectionTester::new(ProtocolConnectionProbe::new(Arc::new(
        StaticCredentialResolver,
    )));
    block_on(tester.test(&http, CancellationToken::new())).unwrap();
    server.join().unwrap();
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

    fn wait_for_sleeper_at(&self, deadline: Duration) {
        let deadline = u64::try_from(deadline.as_millis()).unwrap();
        let mut sleepers = self.inner.sleepers.lock().unwrap();
        while !sleepers
            .iter()
            .any(|(registered, _)| *registered == deadline)
        {
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

#[derive(Clone)]
struct DropRecordingConnector {
    drops: Arc<AtomicUsize>,
}

struct DropRecordingConnection(Arc<AtomicUsize>);

impl Drop for DropRecordingConnection {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

impl RemoteConnector for DropRecordingConnector {
    type Connection = DropRecordingConnection;

    fn connect<'a>(
        &'a self,
        _profile: &'a ConnectionProfile,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Self::Connection, RemoteErrorCategory>> {
        Box::pin(async move { Ok(DropRecordingConnection(self.drops.clone())) })
    }
}

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

#[test]
fn idle_connection_closes_at_deadline_without_another_acquire() {
    let runtime = ManualRuntime::default();
    let drops = Arc::new(AtomicUsize::new(0));
    let pool = ProviderPool::with_runtime(
        profile(
            RemoteProtocol::Sftp,
            SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
        ),
        DropRecordingConnector {
            drops: drops.clone(),
        },
        runtime.clone(),
        PoolLimits::default(),
    );
    let lease = block_on(pool.acquire(CancellationToken::new())).unwrap();
    drop(lease);
    runtime.wait_for_sleeper_at(Duration::from_secs(60));
    runtime.advance(Duration::from_secs(59));
    assert_eq!(drops.load(Ordering::Acquire), 0);
    runtime.advance(Duration::from_secs(1));
    while drops.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }
    assert_eq!(pool.stats().connections(), 0);
    assert_eq!(drops.load(Ordering::Acquire), 1);
    drop(pool);
}

#[test]
fn dropping_pool_closes_idle_connection_and_joins_maintenance() {
    let runtime = ManualRuntime::default();
    let drops = Arc::new(AtomicUsize::new(0));
    let pool = ProviderPool::with_runtime(
        profile(
            RemoteProtocol::Sftp,
            SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
        ),
        DropRecordingConnector {
            drops: drops.clone(),
        },
        runtime,
        PoolLimits::default(),
    );
    drop(block_on(pool.acquire(CancellationToken::new())).unwrap());
    drop(pool);
    assert_eq!(drops.load(Ordering::Acquire), 1);
}

impl fmt::Debug for ManualRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManualRuntime")
            .field("now", &self.now())
            .finish_non_exhaustive()
    }
}
