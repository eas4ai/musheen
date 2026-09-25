use futures_lite::future::block_on;
use musheen_core::{
    BoxFuture, CancellationToken, MutationRequest, PageRequest, ProviderId, ResourceLimits, Store,
};
use musheen_desktop::remote::{
    ConnectionProfile, CredentialResolver, HostKeyPolicy, RemoteHost, RemoteProtocol,
    SecurityPolicy, TlsPolicy, ftp_store_from_profile, http_store_from_profile,
    sftp_store_from_profile, webdav_store_from_profile,
};
use musheen_desktop::{ConnectionId, CredentialReference, SecretBuffer};
use musheen_ops::{EventGeneration, JobId, StagingPath};
use std::process::Command;
use std::thread;
use std::time::Duration;

const USERNAME: &str = "musheen";
const PASSWORD: &str = "musheen-pass";
const FIXTURE_PATH: &str = "/fixtures/range.txt";
const FIXTURE_PREFIX: &[u8] = b"remote fixture";

struct StaticCredentials;

impl CredentialResolver for StaticCredentials {
    fn resolve<'a>(
        &'a self,
        _reference: &'a CredentialReference,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<SecretBuffer, musheen_desktop::remote::RemoteErrorCategory>> {
        Box::pin(async { Ok(SecretBuffer::new(PASSWORD.as_bytes().to_vec())) })
    }
}

fn live_tests_enabled() -> bool {
    std::env::var_os("MUSHEEN_REMOTE_LIVE").is_some()
}

fn profile(
    name: &str,
    protocol: RemoteProtocol,
    port: u16,
    path: &str,
    security: SecurityPolicy,
) -> ConnectionProfile {
    let connection_id =
        ConnectionId::new(format!("live-{name}")).expect("the live-test connection ID is valid");
    ConnectionProfile::new(
        connection_id.clone(),
        format!("Live {name}"),
        protocol,
        RemoteHost::new(protocol, "127.0.0.1").expect("the loopback host is valid"),
        Some(port),
        path,
        Some(USERNAME),
        Some(CredentialReference::persistent(connection_id)),
        security,
        None,
    )
    .expect("the live-test profile is valid")
}

fn provider(name: &str) -> ProviderId {
    ProviderId::new(format!("remote-live-{name}")).expect("the live-test provider ID is valid")
}

fn endpoint_port(variable: &str) -> u16 {
    let endpoint = std::env::var(variable).expect("the live harness supplies the endpoint");
    endpoint
        .trim_end_matches('/')
        .rsplit_once(':')
        .expect("the live endpoint contains a port")
        .1
        .parse()
        .expect("the live endpoint port is valid")
}

fn decode_sha256(variable: &str) -> [u8; 32] {
    let value = std::env::var(variable).expect("the live harness supplies the SHA-256 pin");
    assert_eq!(
        value.len(),
        64,
        "{variable} must contain 32 hexadecimal bytes"
    );
    let mut bytes = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair).expect("the pin contains UTF-8 hex digits");
        bytes[index] = u8::from_str_radix(pair, 16).expect("the pin contains hexadecimal bytes");
    }
    bytes
}

#[test]
fn ftp_http_and_webdav_profiles_reach_live_services() {
    if !live_tests_enabled() {
        return;
    }

    let credentials = StaticCredentials;
    let ftp_profile = profile(
        "ftp",
        RemoteProtocol::Ftp,
        endpoint_port("MUSHEEN_LIVE_FTP_ENDPOINT"),
        "/",
        SecurityPolicy::PlaintextConfirmed,
    );
    let ftp = block_on(ftp_store_from_profile(
        provider("ftp"),
        &ftp_profile,
        &credentials,
        CancellationToken::new(),
    ))
    .expect("the FTP profile connects");
    let ftp_bytes = block_on(
        ftp.read_range(
            &ftp.path(FIXTURE_PATH)
                .expect("the FTP fixture path is valid"),
            0..FIXTURE_PREFIX.len() as u64,
            CancellationToken::new(),
        ),
    )
    .expect("FTP supports bounded range reads");
    assert_eq!(ftp_bytes, FIXTURE_PREFIX);
    let ftp_root = block_on(ftp.read_directory(
        &ftp.root_path(),
        PageRequest::first(&ResourceLimits::default()),
        CancellationToken::new(),
    ))
    .expect("FTP lists the root directory");
    assert!(
        ftp_root
            .items()
            .iter()
            .any(|item| item.display_name().as_str() == "fixtures")
    );

    let ftps_profile = profile(
        "ftps",
        RemoteProtocol::Ftps,
        endpoint_port("MUSHEEN_LIVE_FTPS_ENDPOINT"),
        "/",
        SecurityPolicy::Tls(TlsPolicy::SystemRoots),
    );
    let ftps = block_on(ftp_store_from_profile(
        provider("ftps"),
        &ftps_profile,
        &credentials,
        CancellationToken::new(),
    ))
    .expect("the system-root FTPS profile connects");
    let ftps_bytes = block_on(
        ftps.read_range(
            &ftps
                .path(FIXTURE_PATH)
                .expect("the FTPS fixture path is valid"),
            0..FIXTURE_PREFIX.len() as u64,
            CancellationToken::new(),
        ),
    )
    .expect("FTPS supports bounded range reads over a verified data channel");
    assert_eq!(ftps_bytes, FIXTURE_PREFIX);

    let sftp_profile = profile(
        "sftp",
        RemoteProtocol::Sftp,
        endpoint_port("MUSHEEN_LIVE_SFTP_ENDPOINT"),
        "/srv/remote",
        SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(decode_sha256(
            "MUSHEEN_LIVE_SSH_SHA256",
        ))),
    );
    let sftp = block_on(sftp_store_from_profile(
        provider("sftp"),
        &sftp_profile,
        &credentials,
        CancellationToken::new(),
    ))
    .expect("the pinned SFTP profile connects");
    let sftp_bytes = block_on(
        sftp.read_range(
            &sftp
                .path(FIXTURE_PATH)
                .expect("the SFTP fixture path is valid"),
            0..FIXTURE_PREFIX.len() as u64,
            CancellationToken::new(),
        ),
    )
    .expect("SFTP supports bounded range reads");
    assert_eq!(sftp_bytes, FIXTURE_PREFIX);
    let sftp_root = block_on(sftp.read_directory(
        &sftp.root_path(),
        PageRequest::first(&ResourceLimits::default()),
        CancellationToken::new(),
    ))
    .expect("SFTP lists the root directory");
    let sftp_names = sftp_root
        .items()
        .iter()
        .map(|item| item.display_name().as_str())
        .collect::<Vec<_>>();
    assert!(sftp_names.contains(&"fixtures"), "listed {sftp_names:?}");

    let mut page = block_on(sftp.read_directory(
        &sftp.path("/paged").expect("the paged SFTP path is valid"),
        PageRequest::new(64, None).expect("the bounded page request is valid"),
        CancellationToken::new(),
    ))
    .expect("the first delayed SFTP page loads");
    let mut item_count = page.items().len();
    while let Some(request) = page.next_request() {
        thread::sleep(Duration::from_millis(5));
        page = block_on(sftp.read_directory(
            &sftp.path("/paged").expect("the paged SFTP path is valid"),
            request,
            CancellationToken::new(),
        ))
        .expect("the next delayed SFTP page loads");
        item_count += page.items().len();
    }
    assert_eq!(item_count, 520);

    let container = std::env::var("MUSHEEN_REMOTE_CONTAINER")
        .expect("the live harness supplies its container name");
    let disconnect = Command::new("docker")
        .args([
            "exec",
            &container,
            "pkill",
            "--signal",
            "KILL",
            "--full",
            "sshd: musheen",
        ])
        .status()
        .expect("the hostile-network fixture invokes Docker");
    assert!(
        disconnect.success(),
        "the active SFTP session is disconnected"
    );
    thread::sleep(Duration::from_millis(50));
    let reconnected = block_on(
        sftp.read_range(
            &sftp
                .path(FIXTURE_PATH)
                .expect("the reconnect fixture path is valid"),
            0..FIXTURE_PREFIX.len() as u64,
            CancellationToken::new(),
        ),
    )
    .expect("SFTP reconnects after the live transport is severed");
    assert_eq!(reconnected, FIXTURE_PREFIX);

    let http_profile = profile(
        "http",
        RemoteProtocol::Http,
        endpoint_port("MUSHEEN_LIVE_HTTP_URL"),
        "/",
        SecurityPolicy::PlaintextConfirmed,
    );
    let http = block_on(http_store_from_profile(
        provider("http"),
        &http_profile,
        &credentials,
        CancellationToken::new(),
    ))
    .expect("the HTTP profile connects");
    let http_bytes = block_on(
        http.read_range(
            &http
                .path(FIXTURE_PATH)
                .expect("the HTTP fixture path is valid"),
            0..FIXTURE_PREFIX.len() as u64,
            CancellationToken::new(),
        ),
    )
    .expect("HTTP supports bounded range reads");
    assert_eq!(http_bytes, FIXTURE_PREFIX);

    let webdav_profile = profile(
        "webdav",
        RemoteProtocol::WebDav,
        endpoint_port("MUSHEEN_LIVE_WEBDAV_URL"),
        "/",
        SecurityPolicy::Tls(TlsPolicy::PinnedSha256(decode_sha256(
            "MUSHEEN_LIVE_TLS_SHA256",
        ))),
    );
    let webdav = block_on(webdav_store_from_profile(
        provider("webdav"),
        &webdav_profile,
        &credentials,
        CancellationToken::new(),
    ))
    .expect("the pinned WebDAV profile connects");
    let webdav_bytes = block_on(
        webdav.read_range(
            &webdav
                .path(FIXTURE_PATH)
                .expect("the WebDAV fixture path is valid"),
            0..FIXTURE_PREFIX.len() as u64,
            CancellationToken::new(),
        ),
    )
    .expect("WebDAV supports bounded range reads");
    assert_eq!(webdav_bytes, FIXTURE_PREFIX);

    let created = webdav
        .path("/fixtures/contract-empty.txt")
        .expect("the WebDAV mutation path is valid");
    block_on(webdav.mutate(
        MutationRequest::CreateFile {
            path: created.clone(),
        },
        CancellationToken::new(),
    ))
    .expect("WebDAV creates a file");
    assert!(
        webdav
            .resolve_item(&created)
            .expect("the created WebDAV file resolves")
            .is_some()
    );
    block_on(webdav.mutate(
        MutationRequest::PermanentDelete { target: created },
        CancellationToken::new(),
    ))
    .expect("WebDAV deletes the created file");

    let replacement_webdav = webdav
        .path("/fixtures/external-replacement.txt")
        .expect("the WebDAV replacement path is valid");
    let replacement_sftp = sftp
        .path("/fixtures/external-replacement.txt")
        .expect("the SFTP replacement path is valid");
    block_on(webdav.mutate(
        MutationRequest::CreateFile {
            path: replacement_webdav.clone(),
        },
        CancellationToken::new(),
    ))
    .expect("WebDAV creates the replacement fixture");
    let before = webdav
        .resolve_item(&replacement_webdav)
        .expect("the original WebDAV identity resolves")
        .expect("the original WebDAV fixture exists");
    block_on(sftp.mutate(
        MutationRequest::PermanentDelete {
            target: replacement_sftp.clone(),
        },
        CancellationToken::new(),
    ))
    .expect("SFTP removes the WebDAV-created fixture externally");
    block_on(sftp.mutate(
        MutationRequest::CreateFile {
            path: replacement_sftp.clone(),
        },
        CancellationToken::new(),
    ))
    .expect("SFTP replaces the fixture externally");
    let after = webdav
        .resolve_item(&replacement_webdav)
        .expect("the replacement WebDAV identity resolves")
        .expect("the replacement WebDAV fixture exists");
    assert_ne!(before.id(), after.id());
    block_on(sftp.mutate(
        MutationRequest::PermanentDelete {
            target: replacement_sftp,
        },
        CancellationToken::new(),
    ))
    .expect("SFTP removes the replacement fixture");
}

#[test]
fn reviewed_sftp_source_is_removed_after_quarantine_check() {
    if !live_tests_enabled() {
        return;
    }

    let profile = profile(
        "sftp-cleanup",
        RemoteProtocol::Sftp,
        endpoint_port("MUSHEEN_LIVE_SFTP_ENDPOINT"),
        "/srv/remote",
        SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(decode_sha256(
            "MUSHEEN_LIVE_SSH_SHA256",
        ))),
    );
    let store = block_on(sftp_store_from_profile(
        provider("sftp-cleanup"),
        &profile,
        &StaticCredentials,
        CancellationToken::new(),
    ))
    .expect("the SFTP fixture connects");
    assert!(store.supports_reviewed_source_removal());
    let path = store
        .path(&format!(
            "/fixtures/reviewed-cleanup-{}",
            std::process::id()
        ))
        .expect("the fixture path is valid");
    let local = tempfile::tempdir().expect("the local fixture directory exists");
    let payload = local.path().join("payload");
    std::fs::write(&payload, FIXTURE_PREFIX).expect("the fixture payload is written");
    let staging = StagingPath::for_slash_key_destination_with_nonce(
        &path,
        JobId::new(900).expect("the fixture job ID is valid"),
        EventGeneration::new(0),
        StagingPath::unique_nonce(),
    )
    .expect("the staging path is valid");
    block_on(store.upload_staging_from_local(&payload, &staging, CancellationToken::new()))
        .expect("the fixture payload is staged");
    block_on(store.publish_staging_noreplace(&staging, &path, CancellationToken::new()))
        .expect("the fixture source is published");
    let identity = store
        .resolve_item(&path)
        .expect("the fixture resolves")
        .expect("the fixture exists")
        .id()
        .clone();

    block_on(store.delete_if_unchanged(&path, &identity, CancellationToken::new()))
        .expect("the reviewed source is quarantined, checked, and removed");

    assert!(
        store
            .resolve_item(&path)
            .expect("the original path resolves")
            .is_none()
    );
}

#[test]
fn reviewed_sftp_cleanup_keeps_source_on_identity_mismatch() {
    if !live_tests_enabled() {
        return;
    }

    let profile = profile(
        "sftp-mismatch",
        RemoteProtocol::Sftp,
        endpoint_port("MUSHEEN_LIVE_SFTP_ENDPOINT"),
        "/srv/remote",
        SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(decode_sha256(
            "MUSHEEN_LIVE_SSH_SHA256",
        ))),
    );
    let store = block_on(sftp_store_from_profile(
        provider("sftp-mismatch"),
        &profile,
        &StaticCredentials,
        CancellationToken::new(),
    ))
    .expect("the SFTP fixture connects");
    let source = store
        .path(&format!("/fixtures/mismatch-{}", std::process::id()))
        .expect("the source path is valid");
    block_on(store.mutate(
        MutationRequest::CreateFile {
            path: source.clone(),
        },
        CancellationToken::new(),
    ))
    .expect("the source is created");
    let unrelated = store
        .resolve_item(&store.path(FIXTURE_PATH).expect("the fixture path is valid"))
        .expect("the fixture resolves")
        .expect("the fixture exists");

    let error =
        block_on(store.delete_if_unchanged(&source, unrelated.id(), CancellationToken::new()))
            .expect_err("a different reviewed identity cannot be deleted");

    assert_eq!(
        error.category(),
        musheen_desktop::remote::RemoteErrorCategory::Conflict
    );
    assert!(error.recovery_path().is_none());
    assert!(
        store
            .resolve_item(&source)
            .expect("the source resolves")
            .is_some()
    );
}

#[test]
fn reviewed_webdav_source_is_removed_after_quarantine_check() {
    if !live_tests_enabled() {
        return;
    }

    let profile = profile(
        "webdav-cleanup",
        RemoteProtocol::WebDav,
        endpoint_port("MUSHEEN_LIVE_WEBDAV_URL"),
        "/",
        SecurityPolicy::Tls(TlsPolicy::PinnedSha256(decode_sha256(
            "MUSHEEN_LIVE_TLS_SHA256",
        ))),
    );
    let store = block_on(webdav_store_from_profile(
        provider("webdav-cleanup"),
        &profile,
        &StaticCredentials,
        CancellationToken::new(),
    ))
    .expect("the WebDAV fixture connects");
    assert!(store.supports_reviewed_source_removal());
    let path = store
        .path(&format!("/fixtures/webdav-cleanup-{}", std::process::id()))
        .expect("the fixture path is valid");
    block_on(store.mutate(
        MutationRequest::CreateFile { path: path.clone() },
        CancellationToken::new(),
    ))
    .expect("the fixture source is created");
    let identity = store
        .resolve_item(&path)
        .expect("the fixture resolves")
        .expect("the fixture exists")
        .id()
        .clone();

    block_on(store.delete_if_unchanged(&path, &identity, CancellationToken::new()))
        .expect("the WebDAV source is quarantined, checked, and removed");

    assert!(
        store
            .resolve_item(&path)
            .expect("the original path resolves")
            .is_none()
    );
}
