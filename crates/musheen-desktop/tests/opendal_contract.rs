use futures_lite::future::block_on;
use musheen_core::{
    BoxFuture, CancellationToken, CapabilityKind, CapabilityState, MutationRequest, PageRequest,
    ProviderId, ResourceLimits, Store, StoreError,
};
use musheen_desktop::remote::{
    ConnectionProfile, CredentialResolver, HostKeyPolicy, OpendalStore, ProxyKind, ProxySettings,
    RemoteCasePolicy, RemoteErrorCategory, RemoteErrorContext, RemoteHost, RemoteMutationPolicy,
    RemoteProtocol, SecurityPolicy, TlsPolicy, classify_opendal_error, ftp_store_from_profile,
    http_store_from_profile, sftp_store_from_profile, webdav_store_from_profile,
};
use musheen_desktop::{ConnectionId, CredentialReference, SecretBuffer};
use musheen_ops::{EventGeneration, JobId, StagingPath};
use opendal::{
    Error, ErrorKind, Operator,
    services::{Http, Memory},
};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

struct NoCredentials;

impl CredentialResolver for NoCredentials {
    fn resolve<'a>(
        &'a self,
        _reference: &'a musheen_desktop::CredentialReference,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<musheen_desktop::SecretBuffer, RemoteErrorCategory>> {
        Box::pin(async { Err(RemoteErrorCategory::Authentication) })
    }
}

struct StaticCredentials(&'static [u8]);

impl CredentialResolver for StaticCredentials {
    fn resolve<'a>(
        &'a self,
        _reference: &'a CredentialReference,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<SecretBuffer, RemoteErrorCategory>> {
        Box::pin(async move { Ok(SecretBuffer::new(self.0.to_vec())) })
    }
}

fn profile(
    protocol: RemoteProtocol,
    security: SecurityPolicy,
    proxy: Option<ProxySettings>,
) -> ConnectionProfile {
    ConnectionProfile::new(
        ConnectionId::new(format!("{protocol:?}").to_ascii_lowercase())
            .expect("the connection ID is valid"),
        "contract",
        protocol,
        RemoteHost::new(protocol, "127.0.0.1").expect("the host is valid"),
        None,
        "/",
        None::<Box<str>>,
        None,
        security,
        proxy,
    )
    .expect("the profile is valid")
}

fn credentialed_profile(protocol: RemoteProtocol, security: SecurityPolicy) -> ConnectionProfile {
    let connection_id =
        ConnectionId::new(format!("credentialed-{protocol:?}").to_ascii_lowercase())
            .expect("the connection ID is valid");
    ConnectionProfile::new(
        connection_id.clone(),
        "credentialed contract",
        protocol,
        RemoteHost::new(protocol, "127.0.0.1").expect("the host is valid"),
        Some(9),
        "/",
        Some("contract-user"),
        Some(CredentialReference::persistent(connection_id)),
        security,
        None,
    )
    .expect("the profile is valid")
}

fn memory_operator() -> Operator {
    Operator::new(Memory::default()).expect("the memory backend builds")
}

fn store(
    operator: Operator,
    protocol: RemoteProtocol,
    case_policy: RemoteCasePolicy,
    mutation_policy: RemoteMutationPolicy,
) -> OpendalStore {
    OpendalStore::from_operator(
        ProviderId::new("remote-contract").expect("the provider ID is valid"),
        protocol,
        operator,
        case_policy,
        mutation_policy,
    )
    .expect("the adapter accepts a valid OpenDAL operator")
}

#[test]
fn streaming_upload_writes_only_a_new_owned_staging_object() {
    let operator = memory_operator();
    let store = store(
        operator.clone(),
        RemoteProtocol::WebDav,
        RemoteCasePolicy::Sensitive,
        RemoteMutationPolicy::CapabilitiesVerified,
    );
    let scratch = tempfile::tempdir().unwrap();
    let local = scratch.path().join("source.bin");
    let payload = vec![0x5a; 2 * 1024 * 1024 + 7];
    std::fs::write(&local, &payload).unwrap();
    let destination = store.path("/target.bin").unwrap();
    let staging = StagingPath::for_slash_key_destination_with_nonce(
        &destination,
        JobId::new(5).unwrap(),
        EventGeneration::new(0),
        [0x31; 16],
    )
    .unwrap();

    let written =
        block_on(store.upload_staging_from_local(&local, &staging, CancellationToken::new()))
            .unwrap();

    assert_eq!(written, payload.len() as u64);
    assert_eq!(
        block_on(operator.read(".musheen-stage-v1-5-0-31313131313131313131313131313131"))
            .unwrap()
            .to_vec(),
        payload
    );
    assert!(block_on(operator.stat("target.bin")).is_err());
    assert_eq!(
        block_on(store.upload_staging_from_local(&local, &staging, CancellationToken::new()))
            .unwrap_err()
            .category(),
        RemoteErrorCategory::Conflict
    );
    assert_eq!(
        block_on(operator.read(".musheen-stage-v1-5-0-31313131313131313131313131313131"))
            .unwrap()
            .to_vec(),
        payload
    );
}

#[test]
fn upload_rejects_read_only_and_cancelled_connections_without_writing() {
    let operator = memory_operator();
    let store = store(
        operator.clone(),
        RemoteProtocol::Http,
        RemoteCasePolicy::Unknown,
        RemoteMutationPolicy::ReadOnly,
    );
    let scratch = tempfile::tempdir().unwrap();
    let local = scratch.path().join("source.bin");
    std::fs::write(&local, b"payload").unwrap();
    let destination = store.path("/target.bin").unwrap();
    let staging = StagingPath::for_slash_key_destination_with_nonce(
        &destination,
        JobId::new(6).unwrap(),
        EventGeneration::new(0),
        [0x42; 16],
    )
    .unwrap();

    assert_eq!(
        block_on(store.upload_staging_from_local(&local, &staging, CancellationToken::new()))
            .unwrap_err()
            .category(),
        RemoteErrorCategory::Unsupported
    );
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        block_on(store.upload_staging_from_local(&local, &staging, cancellation))
            .unwrap_err()
            .category(),
        RemoteErrorCategory::Cancelled
    );
    assert!(
        block_on(operator.stat(".musheen-stage-v1-6-0-42424242424242424242424242424242")).is_err()
    );
}

#[test]
fn range_download_is_bounded_and_never_replaces_a_local_file() {
    let operator = memory_operator();
    let payload = vec![0x7f; 2 * 1024 * 1024 + 7];
    block_on(operator.write("source.bin", payload.clone())).unwrap();
    let store = store(
        operator,
        RemoteProtocol::WebDav,
        RemoteCasePolicy::Sensitive,
        RemoteMutationPolicy::ReadOnly,
    );
    let source = store.path("/source.bin").unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let local = scratch.path().join("download.bin");

    assert_eq!(
        block_on(store.download_to_new_local(
            &source,
            &local,
            payload.len() as u64 - 1,
            CancellationToken::new()
        ))
        .unwrap_err()
        .category(),
        RemoteErrorCategory::Quota
    );
    assert!(!local.exists());
    assert_eq!(
        block_on(store.download_to_new_local(
            &source,
            &local,
            payload.len() as u64,
            CancellationToken::new()
        ))
        .unwrap(),
        payload.len() as u64
    );
    assert_eq!(std::fs::read(&local).unwrap(), payload);
    assert_eq!(
        block_on(store.download_to_new_local(
            &source,
            &local,
            payload.len() as u64,
            CancellationToken::new()
        ))
        .unwrap_err()
        .category(),
        RemoteErrorCategory::Conflict
    );
    assert_eq!(std::fs::read(&local).unwrap(), payload);
}

#[test]
fn paged_enumeration_range_reads_and_replacement_identity_share_one_adapter() {
    let operator = memory_operator();
    block_on(async {
        for index in 0..520 {
            operator
                .write(
                    &format!("item-{index:04}.txt"),
                    format!("payload-{index:04}"),
                )
                .await
                .expect("the fixture object is written");
        }
    });
    let store = store(
        operator.clone(),
        RemoteProtocol::WebDav,
        RemoteCasePolicy::Sensitive,
        RemoteMutationPolicy::CapabilitiesVerified,
    );
    let root = store.root_path();
    let first = block_on(store.read_directory(
        &root,
        PageRequest::first(&ResourceLimits::default()),
        CancellationToken::new(),
    ))
    .expect("the first page loads");
    assert_eq!(first.items().len(), 512);
    let second = block_on(store.read_directory(
        &root,
        first.next_request().expect("the second page has a cursor"),
        CancellationToken::new(),
    ))
    .expect("the second page loads");
    assert_eq!(second.items().len(), 8);
    assert!(second.next_request().is_none());

    let item = first
        .items()
        .iter()
        .find(|item| item.display_name().as_str() == "item-0000.txt")
        .expect("the first fixture is listed")
        .clone();
    let bytes = block_on(store.read_range(item.path(), 2..9, CancellationToken::new()))
        .expect("the byte range loads");
    assert_eq!(bytes, b"yload-0");

    let before = store
        .resolve_item(item.path())
        .expect("the first identity resolves")
        .expect("the item exists");
    block_on(operator.write("item-0000.txt", "replacement"))
        .expect("the object is replaced externally");
    let after = store
        .resolve_item(item.path())
        .expect("the replacement identity resolves")
        .expect("the replacement exists");
    assert_ne!(before.id(), after.id());
}

#[test]
fn nested_directory_locations_use_directory_form_and_bind_continuations() {
    let operator = memory_operator();
    block_on(async {
        operator
            .create_dir("alpha/")
            .await
            .expect("the first directory is created");
        operator
            .create_dir("beta/")
            .await
            .expect("the second directory is created");
        operator
            .write("alpha/one", "1")
            .await
            .expect("the first child is written");
        operator
            .write("alpha/two", "2")
            .await
            .expect("the second child is written");
        operator
            .write("beta/other", "3")
            .await
            .expect("the unrelated child is written");
    });
    let store = store(
        operator,
        RemoteProtocol::WebDav,
        RemoteCasePolicy::Sensitive,
        RemoteMutationPolicy::CapabilitiesVerified,
    );
    let alpha = store.path("/alpha").expect("the first path is valid");
    let beta = store.path("/beta").expect("the second path is valid");
    let first = block_on(store.read_directory(
        &alpha,
        PageRequest::new(1, None).expect("the page request is valid"),
        CancellationToken::new(),
    ))
    .expect("the nested directory is listed");
    assert_eq!(first.items().len(), 1);
    let continuation = first.next_request().expect("another child remains");

    assert!(matches!(
        block_on(store.read_directory(&beta, continuation.clone(), CancellationToken::new(),)),
        Err(StoreError::InvalidContinuation)
    ));
    let second = block_on(store.read_directory(&alpha, continuation, CancellationToken::new()))
        .expect("the cursor remains valid for its original directory");
    assert_eq!(second.items().len(), 1);
}

#[test]
fn cancellation_case_policy_and_http_mutation_proof_are_explicit() {
    let operator = memory_operator();
    block_on(operator.write("fixture", "payload")).expect("the fixture is written");
    let http = store(
        operator.clone(),
        RemoteProtocol::Http,
        RemoteCasePolicy::Unknown,
        RemoteMutationPolicy::ReadOnly,
    );
    let root = http.root_path();
    assert!(matches!(
        http.capabilities(&root)
            .get(CapabilityKind::CaseSensitivity),
        CapabilityState::Unknown(_)
    ));
    assert!(matches!(
        http.validate_mutation(&MutationRequest::CreateFile {
            path: http.path("/created").expect("the path is valid")
        }),
        Err(StoreError::Unsupported { .. })
    ));

    let verified = store(
        operator,
        RemoteProtocol::Http,
        RemoteCasePolicy::Insensitive,
        RemoteMutationPolicy::CapabilitiesVerified,
    );
    assert!(matches!(
        verified
            .capabilities(&verified.root_path())
            .get(CapabilityKind::CaseSensitivity),
        CapabilityState::Unsupported(_)
    ));
    verified
        .validate_mutation(&MutationRequest::CreateFile {
            path: verified.path("/created").expect("the path is valid"),
        })
        .expect("a verified writable HTTP service may expose mutation");

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let error = block_on(verified.read_range(
        &verified.path("/fixture").expect("the path is valid"),
        0..1,
        cancellation,
    ))
    .expect_err("a cancelled range read is refused");
    assert_eq!(error.category(), RemoteErrorCategory::Cancelled);
}

#[test]
fn writable_location_requires_a_backend_write_capability() {
    opendal::install_default();
    let operator = Operator::new(Http::default().endpoint("http://127.0.0.1:9"))
        .expect("the HTTP backend configuration is valid");
    let store = store(
        operator,
        RemoteProtocol::Http,
        RemoteCasePolicy::Unknown,
        RemoteMutationPolicy::CapabilitiesVerified,
    );

    assert!(matches!(
        store
            .location_writable(&store.root_path())
            .expect("the capability check succeeds"),
        CapabilityState::Unsupported(_)
    ));
}

#[test]
fn cancellation_aborts_a_range_read_after_transport_work_starts() {
    opendal::install_default();
    let listener = TcpListener::bind("127.0.0.1:0").expect("the fixture binds");
    let address = listener.local_addr().expect("the fixture has an address");
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("the fixture accepts a request");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("the read timeout is set");
        let mut request = [0_u8; 2048];
        let _ = stream.read(&mut request);
        started_tx.send(()).expect("the test observes the request");
        thread::sleep(Duration::from_secs(2));
        let _ = stream.write_all(
            b"HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nContent-Range: bytes 0-0/1\r\n\r\nx",
        );
    });
    let operator = Operator::new(Http::default().endpoint(&format!("http://{address}/")))
        .expect("the HTTP backend configuration is valid");
    let store = store(
        operator,
        RemoteProtocol::Http,
        RemoteCasePolicy::Sensitive,
        RemoteMutationPolicy::ReadOnly,
    );
    let cancellation = CancellationToken::new();
    let canceller = cancellation.clone();
    let cancel_thread = thread::spawn(move || {
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the read reaches the transport");
        canceller.cancel();
    });
    let error = block_on(store.read_range(
        &store.path("/fixture").expect("the path is valid"),
        0..1,
        cancellation,
    ))
    .expect_err("the in-flight read is cancelled");
    assert_eq!(error.category(), RemoteErrorCategory::Cancelled);
    cancel_thread.join().expect("the cancellation thread exits");
    server.join().expect("the fixture thread exits");
}

#[test]
fn opendal_errors_map_to_operation_safe_categories_without_provider_text() {
    let cases = [
        (
            Error::new(ErrorKind::Unexpected, "secret-bearing transient detail").set_temporary(),
            RemoteErrorContext::Read,
            RemoteErrorCategory::Retryable,
        ),
        (
            Error::new(ErrorKind::PermissionDenied, "login rejected"),
            RemoteErrorContext::Authentication,
            RemoteErrorCategory::Authentication,
        ),
        (
            Error::new(ErrorKind::AlreadyExists, "exists"),
            RemoteErrorContext::Mutation,
            RemoteErrorCategory::Conflict,
        ),
        (
            Error::new(ErrorKind::Unexpected, "507 quota exceeded"),
            RemoteErrorContext::Mutation,
            RemoteErrorCategory::Quota,
        ),
        (
            Error::new(ErrorKind::PermissionDenied, "forbidden"),
            RemoteErrorContext::Mutation,
            RemoteErrorCategory::Permission,
        ),
        (
            Error::new(ErrorKind::Unsupported, "not implemented"),
            RemoteErrorContext::Read,
            RemoteErrorCategory::Unsupported,
        ),
        (
            Error::new(ErrorKind::ConfigInvalid, "bad endpoint"),
            RemoteErrorContext::Connect,
            RemoteErrorCategory::Permanent,
        ),
    ];

    for (error, context, expected) in cases {
        assert_eq!(classify_opendal_error(&error, context), expected);
    }
}

#[test]
fn profile_construction_fails_closed_for_unrepresentable_security_and_proxy_policy() {
    let provider = || ProviderId::new("profile-contract").expect("the provider ID is valid");
    let pin = [7_u8; 32];
    let credentials = NoCredentials;
    let pinned_ftps = profile(
        RemoteProtocol::Ftps,
        SecurityPolicy::Tls(TlsPolicy::PinnedSha256(pin)),
        None,
    );
    let error = block_on(ftp_store_from_profile(
        provider(),
        &pinned_ftps,
        &credentials,
        CancellationToken::new(),
    ))
    .err()
    .expect("FTPS pinning fails closed because OpenDAL exposes no verifier hook");
    assert_eq!(error.category(), RemoteErrorCategory::Unsupported);

    for pinned_http in [
        profile(
            RemoteProtocol::Http,
            SecurityPolicy::Tls(TlsPolicy::PinnedSha256(pin)),
            None,
        ),
        profile(
            RemoteProtocol::WebDav,
            SecurityPolicy::Tls(TlsPolicy::PinnedSha256(pin)),
            None,
        ),
    ] {
        match pinned_http.protocol() {
            RemoteProtocol::Http => block_on(http_store_from_profile(
                provider(),
                &pinned_http,
                &credentials,
                CancellationToken::new(),
            )),
            RemoteProtocol::WebDav => block_on(webdav_store_from_profile(
                provider(),
                &pinned_http,
                &credentials,
                CancellationToken::new(),
            )),
            _ => unreachable!(),
        }
        .expect("HTTP-backed pinning uses a per-operator verifier");
    }

    let proxy = ProxySettings::new(
        RemoteProtocol::Http,
        ProxyKind::HttpConnect,
        RemoteHost::new(RemoteProtocol::Http, "127.0.0.1").expect("the proxy host is valid"),
        8080,
        None::<Box<str>>,
        None,
    )
    .expect("the proxy settings are valid");
    let proxied = profile(
        RemoteProtocol::Http,
        SecurityPolicy::PlaintextConfirmed,
        Some(proxy),
    );
    let error = block_on(http_store_from_profile(
        provider(),
        &proxied,
        &credentials,
        CancellationToken::new(),
    ))
    .err()
    .expect("a configured proxy is never bypassed");
    assert_eq!(error.category(), RemoteErrorCategory::Unsupported);

    let pinned_sftp = credentialed_profile(
        RemoteProtocol::Sftp,
        SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(pin)),
    );
    let password = StaticCredentials(b"contract-password");
    let error = block_on(sftp_store_from_profile(
        provider(),
        &pinned_sftp,
        &password,
        CancellationToken::new(),
    ))
    .err()
    .expect("the unreachable pinned SFTP service fails to connect");
    assert_ne!(error.category(), RemoteErrorCategory::Unsupported);
}
