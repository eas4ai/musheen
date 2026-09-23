use super::{
    CONNECT_TIMEOUT, ConnectionProfile, HostKeyPolicy, ProxyKind, RemoteError, RemoteErrorCategory,
    RemoteProtocol, SecurityPolicy, TlsPolicy,
};
use crate::{CredentialReference, CredentialVault, LinuxSecretService, SecretBuffer, SecretError};
use base64::Engine as _;
use futures_lite::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use futures_rustls::TlsConnector;
use futures_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use futures_rustls::rustls::crypto::{
    WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
};
use futures_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use futures_rustls::rustls::{
    ClientConfig, DigitallySignedStruct, Error as TlsError, RootCertStore, SignatureScheme,
};
use musheen_core::{BoxFuture, CancellationToken};
use sha2::{Digest, Sha256};
use std::fmt;
use std::future::poll_fn;
use std::sync::Arc;
use std::task::Poll;
use zeroize::Zeroize;

trait AsyncTransport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncTransport for T {}
type Transport = Box<dyn AsyncTransport>;

pub trait CredentialResolver: Send + Sync + 'static {
    fn resolve<'a>(
        &'a self,
        reference: &'a CredentialReference,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<SecretBuffer, RemoteErrorCategory>>;
}

pub struct SecretServiceCredentialResolver(CredentialVault<LinuxSecretService>);

impl Default for SecretServiceCredentialResolver {
    fn default() -> Self {
        Self(CredentialVault::new(LinuxSecretService::default()))
    }
}

impl CredentialResolver for SecretServiceCredentialResolver {
    fn resolve<'a>(
        &'a self,
        reference: &'a CredentialReference,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<SecretBuffer, RemoteErrorCategory>> {
        Box::pin(async move {
            self.0
                .read(reference, cancellation)
                .await
                .map_err(|error| match error {
                    SecretError::Cancelled => RemoteErrorCategory::Cancelled,
                    SecretError::Locked | SecretError::NotFound => {
                        RemoteErrorCategory::Authentication
                    }
                    _ => RemoteErrorCategory::Unavailable,
                })
        })
    }
}

pub trait ConnectionProbe: Send + Sync + 'static {
    fn connect<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteErrorCategory>>;
}

#[derive(Clone)]
pub struct ProtocolConnectionProbe<R = SecretServiceCredentialResolver>(Arc<R>);

impl<R> ProtocolConnectionProbe<R> {
    #[must_use]
    pub fn new(credentials: Arc<R>) -> Self {
        Self(credentials)
    }
}

impl Default for ProtocolConnectionProbe<SecretServiceCredentialResolver> {
    fn default() -> Self {
        Self::new(Arc::new(SecretServiceCredentialResolver::default()))
    }
}

impl<R: CredentialResolver> ConnectionProbe for ProtocolConnectionProbe<R> {
    fn connect<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteErrorCategory>> {
        Box::pin(async move {
            match profile.protocol() {
                RemoteProtocol::Http | RemoteProtocol::WebDav => {
                    probe_http(profile, self.0.as_ref(), cancellation).await
                }
                RemoteProtocol::Ftp | RemoteProtocol::Ftps => {
                    probe_ftp(profile, self.0.as_ref(), cancellation).await
                }
                RemoteProtocol::Sftp => match profile.security() {
                    SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts)
                    | SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(_)) => {
                        Err(RemoteErrorCategory::Unavailable)
                    }
                    _ => Err(RemoteErrorCategory::InvalidProfile),
                },
                RemoteProtocol::Smb | RemoteProtocol::Nfs => Err(RemoteErrorCategory::Unavailable),
            }
        })
    }
}

impl<R> fmt::Debug for ProtocolConnectionProbe<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProtocolConnectionProbe(..)")
    }
}

pub trait ProfileConnectionTest: Send + Sync + 'static {
    fn test<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteError>>;
}

#[derive(Clone, Debug)]
pub struct ProfileConnectionTester<P = ProtocolConnectionProbe> {
    probe: P,
}

impl<P> ProfileConnectionTester<P> {
    #[must_use]
    pub const fn new(probe: P) -> Self {
        Self { probe }
    }
}

impl Default for ProfileConnectionTester<ProtocolConnectionProbe> {
    fn default() -> Self {
        Self::new(ProtocolConnectionProbe::default())
    }
}

impl<P: ConnectionProbe> ProfileConnectionTest for ProfileConnectionTester<P> {
    fn test<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteError>> {
        Box::pin(async move {
            let cancel = cancellation.clone();
            let result =
                futures_lite::future::race(self.probe.connect(profile, cancellation), async move {
                    futures_lite::future::race(
                        async {
                            cancelled(cancel).await;
                            Err(RemoteErrorCategory::Cancelled)
                        },
                        async {
                            async_io::Timer::after(CONNECT_TIMEOUT).await;
                            Err(RemoteErrorCategory::Timeout)
                        },
                    )
                    .await
                })
                .await;
            result.map_err(|category| {
                RemoteError::new(profile.protocol(), category, Some(profile.host().clone()))
            })
        })
    }
}

async fn probe_http<R: CredentialResolver>(
    profile: &ConnectionProfile,
    credentials: &R,
    cancellation: CancellationToken,
) -> Result<(), RemoteErrorCategory> {
    let mut stream = connect_transport(profile, credentials, cancellation.clone()).await?;
    if matches!(profile.security(), SecurityPolicy::Tls(_)) {
        stream = connect_tls(stream, profile).await?;
    }
    let method = if profile.protocol() == RemoteProtocol::WebDav {
        "PROPFIND"
    } else {
        "HEAD"
    };
    let mut request = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        encode_http_path(profile.path()),
        profile.host()
    )
    .into_bytes();
    if profile.protocol() == RemoteProtocol::WebDav {
        request.extend_from_slice(b"Depth: 0\r\n");
    }
    if let Some(reference) = profile.credential() {
        let username = profile
            .username()
            .ok_or(RemoteErrorCategory::InvalidProfile)?;
        let secret = credentials.resolve(reference, cancellation).await?;
        append_basic_auth(&mut request, b"Authorization", username, &secret);
    }
    request.extend_from_slice(b"\r\n");
    let result = stream.write_all(&request).await;
    request.zeroize();
    result.map_err(|_| RemoteErrorCategory::Network)?;
    match read_http_status(&mut stream).await? {
        200..=399 => Ok(()),
        401 | 407 => Err(RemoteErrorCategory::Authentication),
        _ => Err(RemoteErrorCategory::Protocol),
    }
}

async fn probe_ftp<R: CredentialResolver>(
    profile: &ConnectionProfile,
    credentials: &R,
    cancellation: CancellationToken,
) -> Result<(), RemoteErrorCategory> {
    let mut stream = connect_transport(profile, credentials, cancellation.clone()).await?;
    if profile.protocol() == RemoteProtocol::Ftps {
        stream = connect_tls(stream, profile).await?;
    }
    expect_ftp(&mut stream, 220).await?;
    let username = profile.username().unwrap_or("anonymous");
    send_ftp(&mut stream, b"USER ", username.as_bytes()).await?;
    match read_ftp_status(&mut stream).await? {
        230 => {}
        331 => {
            let secret = match profile.credential() {
                Some(reference) => credentials.resolve(reference, cancellation).await?,
                None if username == "anonymous" => SecretBuffer::new(b"musheen@localhost".to_vec()),
                None => return Err(RemoteErrorCategory::Authentication),
            };
            let mut password = secret.expose_secret(<[u8]>::to_vec);
            let result = send_ftp(&mut stream, b"PASS ", &password).await;
            password.zeroize();
            result?;
            expect_ftp(&mut stream, 230).await?;
        }
        _ => return Err(RemoteErrorCategory::Authentication),
    }
    send_ftp(&mut stream, b"CWD ", profile.path().as_bytes()).await?;
    match read_ftp_status(&mut stream).await? {
        200..=299 => Ok(()),
        530 => Err(RemoteErrorCategory::Authentication),
        _ => Err(RemoteErrorCategory::Protocol),
    }
}

async fn connect_transport<R: CredentialResolver>(
    profile: &ConnectionProfile,
    credentials: &R,
    cancellation: CancellationToken,
) -> Result<Transport, RemoteErrorCategory> {
    let target_port = profile.port().unwrap_or_else(|| default_port(profile));
    let endpoint = profile
        .proxy()
        .map_or((profile.host().as_str(), target_port), |proxy| {
            (proxy.host().as_str(), proxy.port())
        });
    let mut stream: Transport = Box::new(
        async_net::TcpStream::connect(endpoint)
            .await
            .map_err(|_| RemoteErrorCategory::Network)?,
    );
    if let Some(proxy) = profile.proxy() {
        let secret = match proxy.credential() {
            Some(reference) => Some(credentials.resolve(reference, cancellation).await?),
            None => None,
        };
        match proxy.kind() {
            ProxyKind::Socks5 => {
                socks5_connect(
                    &mut stream,
                    profile.host().as_str(),
                    target_port,
                    proxy.username(),
                    secret.as_ref(),
                )
                .await?;
            }
            ProxyKind::HttpConnect => {
                http_connect(
                    &mut stream,
                    profile.host().as_str(),
                    target_port,
                    proxy.username(),
                    secret.as_ref(),
                )
                .await?;
            }
        }
    }
    Ok(stream)
}

async fn socks5_connect(
    stream: &mut Transport,
    host: &str,
    port: u16,
    username: Option<&str>,
    secret: Option<&SecretBuffer>,
) -> Result<(), RemoteErrorCategory> {
    let authenticated = username.is_some() || secret.is_some();
    stream
        .write_all(if authenticated {
            &[5, 1, 2]
        } else {
            &[5, 1, 0]
        })
        .await
        .map_err(|_| RemoteErrorCategory::Network)?;
    let mut response = [0; 2];
    stream
        .read_exact(&mut response)
        .await
        .map_err(|_| RemoteErrorCategory::Network)?;
    if response != [5, if authenticated { 2 } else { 0 }] {
        return Err(if response[1] == 0xff {
            RemoteErrorCategory::Authentication
        } else {
            RemoteErrorCategory::Protocol
        });
    }
    if authenticated {
        let username = username.ok_or(RemoteErrorCategory::InvalidProfile)?;
        let secret = secret.ok_or(RemoteErrorCategory::InvalidProfile)?;
        let mut password = secret.expose_secret(<[u8]>::to_vec);
        if username.len() > 255 || password.len() > 255 {
            password.zeroize();
            return Err(RemoteErrorCategory::InvalidProfile);
        }
        let mut auth = vec![1, username.len() as u8];
        auth.extend_from_slice(username.as_bytes());
        auth.push(password.len() as u8);
        auth.extend_from_slice(&password);
        password.zeroize();
        let result = stream.write_all(&auth).await;
        auth.zeroize();
        result.map_err(|_| RemoteErrorCategory::Network)?;
        stream
            .read_exact(&mut response)
            .await
            .map_err(|_| RemoteErrorCategory::Network)?;
        if response != [1, 0] {
            return Err(RemoteErrorCategory::Authentication);
        }
    }
    if host.len() > 255 {
        return Err(RemoteErrorCategory::InvalidProfile);
    }
    let mut request = vec![5, 1, 0, 3, host.len() as u8];
    request.extend_from_slice(host.as_bytes());
    request.extend_from_slice(&port.to_be_bytes());
    stream
        .write_all(&request)
        .await
        .map_err(|_| RemoteErrorCategory::Network)?;
    let mut head = [0; 4];
    stream
        .read_exact(&mut head)
        .await
        .map_err(|_| RemoteErrorCategory::Network)?;
    if head[..2] != [5, 0] {
        return Err(RemoteErrorCategory::Network);
    }
    let address_length = match head[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut length = [0];
            stream
                .read_exact(&mut length)
                .await
                .map_err(|_| RemoteErrorCategory::Protocol)?;
            usize::from(length[0])
        }
        _ => return Err(RemoteErrorCategory::Protocol),
    };
    let mut bound = vec![0; address_length + 2];
    stream
        .read_exact(&mut bound)
        .await
        .map_err(|_| RemoteErrorCategory::Protocol)?;
    Ok(())
}

async fn http_connect(
    stream: &mut Transport,
    host: &str,
    port: u16,
    username: Option<&str>,
    secret: Option<&SecretBuffer>,
) -> Result<(), RemoteErrorCategory> {
    let authority = format!("{host}:{port}");
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n").into_bytes();
    match (username, secret) {
        (None, None) => {}
        (Some(username), Some(secret)) => {
            append_basic_auth(&mut request, b"Proxy-Authorization", username, secret);
        }
        _ => return Err(RemoteErrorCategory::InvalidProfile),
    }
    request.extend_from_slice(b"\r\n");
    let result = stream.write_all(&request).await;
    request.zeroize();
    result.map_err(|_| RemoteErrorCategory::Network)?;
    match read_http_status(stream).await? {
        200..=299 => Ok(()),
        401 | 407 => Err(RemoteErrorCategory::Authentication),
        _ => Err(RemoteErrorCategory::Network),
    }
}

fn append_basic_auth(request: &mut Vec<u8>, header: &[u8], username: &str, secret: &SecretBuffer) {
    let mut source = username.as_bytes().to_vec();
    source.push(b':');
    secret.expose_secret(|value| source.extend_from_slice(value));
    let mut encoded = base64::engine::general_purpose::STANDARD.encode(&source);
    source.zeroize();
    request.extend_from_slice(header);
    request.extend_from_slice(b": Basic ");
    request.extend_from_slice(encoded.as_bytes());
    request.extend_from_slice(b"\r\n");
    encoded.zeroize();
}

async fn connect_tls(
    stream: Transport,
    profile: &ConnectionProfile,
) -> Result<Transport, RemoteErrorCategory> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let provider = Arc::new(futures_rustls::rustls::crypto::ring::default_provider());
    let algorithms = provider.signature_verification_algorithms;
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| RemoteErrorCategory::Tls)?;
    let config = match profile.security() {
        SecurityPolicy::Tls(TlsPolicy::SystemRoots) => {
            builder.with_root_certificates(roots).with_no_client_auth()
        }
        SecurityPolicy::Tls(TlsPolicy::PinnedSha256(pin)) => builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedVerifier {
                pin: *pin,
                algorithms,
            }))
            .with_no_client_auth(),
        _ => return Err(RemoteErrorCategory::InvalidProfile),
    };
    let server_name = ServerName::try_from(profile.host().as_str().to_owned())
        .map_err(|_| RemoteErrorCategory::InvalidProfile)?;
    TlsConnector::from(Arc::new(config))
        .connect(server_name, stream)
        .await
        .map(|stream| Box::new(stream) as Transport)
        .map_err(|_| RemoteErrorCategory::Tls)
}

#[derive(Debug)]
struct PinnedVerifier {
    pin: [u8; 32],
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let actual: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
        if actual == self.pin {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::InvalidCertificate(
                futures_rustls::rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

async fn read_http_status(stream: &mut Transport) -> Result<u16, RemoteErrorCategory> {
    let line = read_line(stream).await?;
    let mut fields = line.split_ascii_whitespace();
    if !fields
        .next()
        .is_some_and(|value| value.starts_with("HTTP/"))
    {
        return Err(RemoteErrorCategory::Protocol);
    }
    let status = fields
        .next()
        .ok_or(RemoteErrorCategory::Protocol)?
        .parse()
        .map_err(|_| RemoteErrorCategory::Protocol)?;
    loop {
        let header = read_line(stream).await?;
        if header == "\r\n" || header == "\n" {
            break;
        }
    }
    Ok(status)
}

async fn read_line(stream: &mut Transport) -> Result<String, RemoteErrorCategory> {
    let mut line = Vec::with_capacity(128);
    while line.len() < 8192 {
        let mut byte = [0];
        stream
            .read_exact(&mut byte)
            .await
            .map_err(|_| RemoteErrorCategory::Protocol)?;
        line.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    String::from_utf8(line).map_err(|_| RemoteErrorCategory::Protocol)
}

async fn send_ftp(
    stream: &mut Transport,
    command: &[u8],
    value: &[u8],
) -> Result<(), RemoteErrorCategory> {
    if value.contains(&b'\r') || value.contains(&b'\n') {
        return Err(RemoteErrorCategory::InvalidProfile);
    }
    stream
        .write_all(command)
        .await
        .map_err(|_| RemoteErrorCategory::Network)?;
    stream
        .write_all(value)
        .await
        .map_err(|_| RemoteErrorCategory::Network)?;
    stream
        .write_all(b"\r\n")
        .await
        .map_err(|_| RemoteErrorCategory::Network)
}

async fn expect_ftp(stream: &mut Transport, expected: u16) -> Result<(), RemoteErrorCategory> {
    match read_ftp_status(stream).await? {
        actual if actual == expected => Ok(()),
        530 => Err(RemoteErrorCategory::Authentication),
        _ => Err(RemoteErrorCategory::Protocol),
    }
}

async fn read_ftp_status(stream: &mut Transport) -> Result<u16, RemoteErrorCategory> {
    loop {
        let line = read_line(stream).await?;
        if line.as_bytes().get(3) == Some(&b' ') {
            return line
                .get(..3)
                .ok_or(RemoteErrorCategory::Protocol)?
                .parse()
                .map_err(|_| RemoteErrorCategory::Protocol);
        }
    }
}

fn encode_http_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn default_port(profile: &ConnectionProfile) -> u16 {
    match profile.protocol() {
        RemoteProtocol::Ftp => 21,
        RemoteProtocol::Ftps => 990,
        RemoteProtocol::Sftp => 22,
        RemoteProtocol::WebDav | RemoteProtocol::Http => {
            if matches!(profile.security(), SecurityPolicy::PlaintextConfirmed) {
                80
            } else {
                443
            }
        }
        RemoteProtocol::Smb => 445,
        RemoteProtocol::Nfs => 2049,
    }
}

async fn cancelled(cancellation: CancellationToken) {
    poll_fn(move |context| {
        if cancellation.is_cancelled() {
            Poll::Ready(())
        } else {
            cancellation.register_waker(context.waker());
            Poll::Pending
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_tls_verifier_rejects_the_wrong_leaf_certificate() {
        let certificate = CertificateDer::from(vec![1, 2, 3, 4]);
        let algorithms = futures_rustls::rustls::crypto::ring::default_provider()
            .signature_verification_algorithms;
        let verifier = PinnedVerifier {
            pin: Sha256::digest(certificate.as_ref()).into(),
            algorithms,
        };
        let server_name = ServerName::try_from("files.example.test").expect("valid DNS name");
        assert!(
            verifier
                .verify_server_cert(
                    &certificate,
                    &[],
                    &server_name,
                    &[],
                    UnixTime::since_unix_epoch(std::time::Duration::ZERO),
                )
                .is_ok()
        );

        let wrong_certificate = CertificateDer::from(vec![4, 3, 2, 1]);
        assert!(
            verifier
                .verify_server_cert(
                    &wrong_certificate,
                    &[],
                    &server_name,
                    &[],
                    UnixTime::since_unix_epoch(std::time::Duration::ZERO),
                )
                .is_err()
        );
    }
}
