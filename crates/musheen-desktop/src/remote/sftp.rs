use super::{
    ConnectionProfile, CredentialResolver, HostKeyPolicy, OpendalStore, RemoteCasePolicy,
    RemoteError, RemoteErrorCategory, RemoteMutationPolicy, RemoteProtocol, SecurityPolicy,
    SshLogin,
};
use musheen_core::{BoxFuture, CancellationToken, ProviderId};
use opendal::raw::{
    OpCopy, OpCreateDir, OpDelete, OpList, OpPresign, OpRead, OpRename, OpStat, OpWrite,
    RpCreateDir, RpPresign, RpRead, RpRename, RpStat, Service, ServiceInfo, Timestamp, oio,
};
use opendal::{
    Buffer, Builder, BytesRange, Capability, Error, ErrorKind, Metadata, MetadataBuilder,
    OperationContext, Operator, Result as OpendalResult,
};
use russh::client;
use russh::keys::{
    HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKeyBase64, PublicKeyOrCertificate,
    decode_secret_key,
};
use russh_sftp::client::{RawSftpSession, error::Error as SftpError};
use russh_sftp::protocol::{File, FileAttributes, OpenFlags, StatusCode};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use super::opendal_store::{RemoteErrorContext, classify_opendal_error, profile_error, run_remote};

const SFTP_CHUNK_SIZE: usize = 32 * 1024;

pub fn sftp_store(
    provider: ProviderId,
    operator: Operator,
    case_policy: RemoteCasePolicy,
) -> Result<OpendalStore, RemoteError> {
    OpendalStore::from_operator(
        provider,
        RemoteProtocol::Sftp,
        operator,
        case_policy,
        RemoteMutationPolicy::CapabilitiesVerified,
    )
}

/// The SSH files and agent an SFTP login reads: ~/.ssh/config, known_hosts,
/// and the agent socket. Tests point these at their own files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshEnvironment {
    pub home: std::path::PathBuf,
    pub config: Option<std::path::PathBuf>,
    pub known_hosts: std::path::PathBuf,
    pub agent_socket: Option<std::path::PathBuf>,
}

impl SshEnvironment {
    /// The current user's ~/.ssh files and `SSH_AUTH_SOCK`.
    #[must_use]
    pub fn for_current_user() -> Self {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let ssh = home.join(".ssh");
        Self {
            config: Some(ssh.join("config")),
            known_hosts: ssh.join("known_hosts"),
            agent_socket: std::env::var_os("SSH_AUTH_SOCK").map(std::path::PathBuf::from),
            home,
        }
    }
}

pub fn sftp_store_from_profile<'a, R: CredentialResolver>(
    provider: ProviderId,
    profile: &'a ConnectionProfile,
    credentials: &'a R,
    cancellation: CancellationToken,
) -> BoxFuture<'a, Result<OpendalStore, RemoteError>> {
    Box::pin(async move {
        let environment = SshEnvironment::for_current_user();
        sftp_store_from_profile_in(provider, profile, credentials, &environment, cancellation).await
    })
}

pub fn sftp_store_from_profile_in<'a, R: CredentialResolver>(
    provider: ProviderId,
    profile: &'a ConnectionProfile,
    credentials: &'a R,
    environment: &'a SshEnvironment,
    cancellation: CancellationToken,
) -> BoxFuture<'a, Result<OpendalStore, RemoteError>> {
    Box::pin(async move {
        if cancellation.is_cancelled() {
            return Err(profile_error(profile, RemoteErrorCategory::Cancelled));
        }
        if profile.protocol() != RemoteProtocol::Sftp || profile.proxy().is_some() {
            return Err(profile_error(profile, RemoteErrorCategory::Unsupported));
        }
        let host_key = match profile.security() {
            SecurityPolicy::Ssh(policy) => policy.clone(),
            _ => return Err(profile_error(profile, RemoteErrorCategory::InvalidProfile)),
        };
        let fail = |category| profile_error(profile, category);
        let route = SshRoute::resolve(profile, environment).map_err(fail)?;
        let login = SshLoginMaterial::load(
            profile,
            credentials,
            environment,
            &route,
            cancellation.clone(),
        )
        .await
        .map_err(fail)?;
        let service = RusshSftpService::new(RusshSftpConfig {
            route,
            root: profile.path().to_owned(),
            login,
            host_key,
            known_hosts: environment.known_hosts.clone(),
            agent_socket: environment.agent_socket.clone(),
        });
        let warm_service = service.clone();
        run_remote(cancellation, async move { warm_service.warm_up().await })
            .await
            .map_err(fail)?
            .map_err(fail)?;
        let operator = Operator::new(RusshSftpBuilder::new(service))
            .map_err(|error| fail(classify_opendal_error(&error, RemoteErrorContext::Connect)))?;
        OpendalStore::from_profile_operator(
            provider,
            profile,
            operator,
            RemoteCasePolicy::Unknown,
            RemoteMutationPolicy::CapabilitiesVerified,
        )
    })
}

impl SshEnvironment {
    /// The parsed ~/.ssh/config, or `None` when there is none.
    fn ssh_config(&self) -> Result<Option<ssh2_config::SshConfig>, RemoteErrorCategory> {
        let Some(path) = &self.config else {
            return Ok(None);
        };
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(RemoteErrorCategory::SshConfigUnreadable),
        };
        ssh2_config::SshConfig::default()
            .parse(
                &mut std::io::BufReader::new(file),
                ssh2_config::ParseRule::ALLOW_UNKNOWN_FIELDS
                    | ssh2_config::ParseRule::ALLOW_UNSUPPORTED_FIELDS,
            )
            .map(Some)
            .map_err(|_| RemoteErrorCategory::SshConfigUnreadable)
    }

    /// `path` with a leading `~/` read from this environment's home.
    fn expand(&self, path: &str) -> PathBuf {
        match path.strip_prefix("~/") {
            Some(rest) => self.home.join(rest),
            None => PathBuf::from(path),
        }
    }
}

/// One SSH server on the way, as ~/.ssh/config resolves it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SshHop {
    host: String,
    port: u16,
    user: String,
    identity_files: Vec<PathBuf>,
}

/// The servers a login passes: the ProxyJump hosts in order, then the
/// connection's server.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SshRoute {
    jumps: Vec<SshHop>,
    target: SshHop,
}

impl SshRoute {
    /// Applies the Host entry of the connection's host: its HostName and
    /// ProxyJump, and its User, Port and IdentityFile where the profile
    /// leaves them empty. Each jump host is resolved through its own entry,
    /// and the first one is reached through its own ProxyJump, as ssh does.
    fn resolve(
        profile: &ConnectionProfile,
        environment: &SshEnvironment,
    ) -> Result<Self, RemoteErrorCategory> {
        let config = environment.ssh_config()?;
        let entry = |host: &str| host_entry(config.as_ref(), host);
        let alias = profile.host().as_str();
        let params = entry(alias)?;
        let user = profile
            .username()
            .map(str::to_owned)
            .or_else(|| params.as_ref().and_then(|params| params.user.clone()))
            .or_else(local_user)
            .ok_or(RemoteErrorCategory::InvalidProfile)?;
        let port = profile
            .port()
            .or_else(|| params.as_ref().and_then(|params| params.port))
            .unwrap_or(22);
        let target = hop(params.as_ref(), alias, &user, port, environment);
        let jumps = params
            .as_ref()
            .and_then(|params| params.proxy_jump.clone())
            .map(|jumps| jump_hops(&jumps, &entry, &target.user, environment, 0))
            .transpose()?
            .unwrap_or_default();
        Ok(Self { jumps, target })
    }
}

/// How many jump hosts a route may pass through a chain of ProxyJump entries.
const MAX_JUMP_DEPTH: usize = 8;

/// The Host entry for `host`. ssh2-config 0.8 reads no Match block: it keeps
/// the options under one as options of the Host entry before it, so an entry
/// that took any is refused rather than used with the wrong values.
fn host_entry(
    config: Option<&ssh2_config::SshConfig>,
    host: &str,
) -> Result<Option<ssh2_config::HostParams>, RemoteErrorCategory> {
    let Some(config) = config else {
        return Ok(None);
    };
    let params = config.query(host);
    if params
        .ignored_fields
        .keys()
        .any(|field| field.eq_ignore_ascii_case("match"))
    {
        return Err(RemoteErrorCategory::SshConfigMatch);
    }
    Ok(Some(params))
}

/// One server on the route, from its Host entry and the values that take
/// precedence over it.
fn hop(
    params: Option<&ssh2_config::HostParams>,
    alias: &str,
    user: &str,
    port: u16,
    environment: &SshEnvironment,
) -> SshHop {
    let local = local_user().unwrap_or_default();
    let host = params
        .and_then(|params| params.host_name.as_deref())
        .map(|name| {
            expand_tokens(
                name,
                &Tokens {
                    host: alias,
                    port,
                    remote_user: user,
                    local_user: &local,
                    home: &environment.home,
                },
            )
        })
        .unwrap_or_else(|| alias.to_owned());
    let tokens = Tokens {
        host: &host,
        port,
        remote_user: user,
        local_user: &local,
        home: &environment.home,
    };
    SshHop {
        identity_files: identity_files(params, environment, &tokens),
        host,
        port,
        user: user.to_owned(),
    }
}

/// The jump hosts a ProxyJump list names, in order. The first one is itself
/// reached through its own entry's ProxyJump, up to `MAX_JUMP_DEPTH` hosts.
fn jump_hops(
    jumps: &[String],
    entry: &dyn Fn(&str) -> Result<Option<ssh2_config::HostParams>, RemoteErrorCategory>,
    default_user: &str,
    environment: &SshEnvironment,
    depth: usize,
) -> Result<Vec<SshHop>, RemoteErrorCategory> {
    let mut hops = Vec::new();
    let named = jumps
        .iter()
        .filter(|jump| !jump.eq_ignore_ascii_case("none"));
    for (index, jump) in named.enumerate() {
        if depth + hops.len() >= MAX_JUMP_DEPTH {
            return Err(RemoteErrorCategory::SshConfigUnreadable);
        }
        let (user, alias, port) = parse_jump(jump)?;
        let params = entry(&alias)?;
        if index == 0
            && let Some(own) = params.as_ref().and_then(|params| params.proxy_jump.clone())
        {
            hops.extend(jump_hops(
                &own,
                entry,
                default_user,
                environment,
                depth + 1,
            )?);
        }
        let user = user
            .or_else(|| params.as_ref().and_then(|params| params.user.clone()))
            .unwrap_or_else(|| default_user.to_owned());
        let port = port
            .or_else(|| params.as_ref().and_then(|params| params.port))
            .unwrap_or(22);
        hops.push(hop(params.as_ref(), &alias, &user, port, environment));
    }
    Ok(hops)
}

/// Reads one ProxyJump host: `[user@]host[:port]` or
/// `ssh://[user@]host[:port]`, with an IPv6 address in brackets, or bare
/// when it has no port.
fn parse_jump(value: &str) -> Result<(Option<String>, String, Option<u16>), RemoteErrorCategory> {
    let unreadable = RemoteErrorCategory::SshConfigUnreadable;
    let value = value.strip_prefix("ssh://").unwrap_or(value);
    let value = value.strip_suffix('/').unwrap_or(value);
    let (user, address) = match value.rsplit_once('@') {
        Some((user, address)) if !user.is_empty() => (Some(user.to_owned()), address),
        Some(_) => return Err(unreadable),
        None => (None, value),
    };
    let port = |text: &str| {
        text.parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or(unreadable)
    };
    let (host, port) = if let Some(rest) = address.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or(unreadable)?;
        match after {
            "" => (host, None),
            _ => (
                host,
                Some(port(after.strip_prefix(':').ok_or(unreadable)?)?),
            ),
        }
    } else {
        match address.split_once(':') {
            Some((host, text)) if !text.contains(':') => (host, Some(port(text)?)),
            // No port, or an IPv6 address written without brackets.
            _ => (address, None),
        }
    };
    if host.is_empty() {
        return Err(unreadable);
    }
    Ok((user, host.to_owned(), port))
}

/// The values ssh_config's `%` tokens stand for.
struct Tokens<'a> {
    host: &'a str,
    port: u16,
    remote_user: &'a str,
    local_user: &'a str,
    home: &'a std::path::Path,
}

/// Expands the `%` tokens of a HostName or IdentityFile value: `%h` the
/// host, `%p` the port, `%r` the remote user, `%u` the local user, `%d` the
/// home folder and `%%` a percent sign. Another token stays as written.
fn expand_tokens(value: &str, tokens: &Tokens<'_>) -> String {
    let mut expanded = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '%' {
            expanded.push(character);
            continue;
        }
        match characters.next() {
            Some('%') => expanded.push('%'),
            Some('h') => expanded.push_str(tokens.host),
            Some('p') => expanded.push_str(&tokens.port.to_string()),
            Some('r') => expanded.push_str(tokens.remote_user),
            Some('u') => expanded.push_str(tokens.local_user),
            Some('d') => expanded.push_str(&tokens.home.to_string_lossy()),
            Some(other) => {
                expanded.push('%');
                expanded.push(other);
            }
            None => expanded.push('%'),
        }
    }
    expanded
}

fn local_user() -> Option<String> {
    std::env::var("USER").ok()
}

fn identity_files(
    params: Option<&ssh2_config::HostParams>,
    environment: &SshEnvironment,
    tokens: &Tokens<'_>,
) -> Vec<PathBuf> {
    params
        .and_then(|params| params.identity_file.clone())
        .unwrap_or_default()
        .into_iter()
        .map(|path| environment.expand(&expand_tokens(&path.to_string_lossy(), tokens)))
        .collect()
}

/// What a login offers the connection's server, loaded before any network
/// I/O so a refused key fails before a server is contacted.
enum SshLoginMaterial {
    Password(Option<Zeroizing<String>>),
    Agent,
    Key(Arc<PrivateKey>),
}

impl SshLoginMaterial {
    async fn load<R: CredentialResolver>(
        profile: &ConnectionProfile,
        credentials: &R,
        environment: &SshEnvironment,
        route: &SshRoute,
        cancellation: CancellationToken,
    ) -> Result<Self, RemoteErrorCategory> {
        let secret_text = |bytes: &[u8]| {
            std::str::from_utf8(bytes)
                .map(|text| Zeroizing::new(text.to_owned()))
                .map_err(|_| RemoteErrorCategory::Authentication)
        };
        let key_text = |bytes: &[u8]| {
            std::str::from_utf8(bytes)
                .map(|text| Zeroizing::new(text.to_owned()))
                .map_err(|_| RemoteErrorCategory::KeyUndecodable)
        };
        let passphrase = match profile.credential() {
            Some(reference) => Some(
                credentials
                    .resolve(reference, cancellation.clone())
                    .await?
                    .expose_secret(secret_text)?,
            ),
            None => None,
        };
        match profile.login() {
            SshLogin::Password => Ok(Self::Password(passphrase)),
            SshLogin::Agent => match &environment.agent_socket {
                Some(socket) if std::os::unix::net::UnixStream::connect(socket).is_ok() => {
                    Ok(Self::Agent)
                }
                _ => Err(RemoteErrorCategory::NoAgent),
            },
            SshLogin::KeyFile { path } => {
                let path = path
                    .as_deref()
                    .map(|path| environment.expand(path))
                    .or_else(|| route.target.identity_files.first().cloned())
                    .ok_or(RemoteErrorCategory::InvalidProfile)?;
                let text = read_key_file(&path)?;
                decode_login_key(&text, passphrase.as_deref().map(String::as_str)).map(Self::Key)
            }
            SshLogin::StoredKey => {
                let reference = profile
                    .stored_key_reference()
                    .ok_or(RemoteErrorCategory::InvalidProfile)?;
                let text = credentials
                    .resolve(&reference, cancellation)
                    .await?
                    .expose_secret(key_text)?;
                decode_login_key(&text, passphrase.as_deref().map(String::as_str)).map(Self::Key)
            }
        }
    }
}

/// The largest private key file Musheen reads; OpenSSH keys are a few
/// kilobytes.
const MAX_KEY_FILE_BYTES: u64 = 64 * 1024;

/// Reads a private key file the user named, refusing one larger than any
/// private key.
fn read_key_file(path: &std::path::Path) -> Result<Zeroizing<String>, RemoteErrorCategory> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| RemoteErrorCategory::KeyUnreadable)?;
    let mut text = Zeroizing::new(String::new());
    file.take(MAX_KEY_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| RemoteErrorCategory::KeyUndecodable)?;
    if text.len() as u64 > MAX_KEY_FILE_BYTES {
        return Err(RemoteErrorCategory::KeyUndecodable);
    }
    Ok(text)
}

/// Decodes a private key Musheen signs with itself: Ed25519 or ECDSA. An RSA
/// key is refused; it signs through the SSH agent only.
fn decode_login_key(
    text: &str,
    passphrase: Option<&str>,
) -> Result<Arc<PrivateKey>, RemoteErrorCategory> {
    if PrivateKey::from_openssh(text).is_ok_and(|key| key.algorithm().is_rsa()) {
        return Err(RemoteErrorCategory::KeyNeedsAgent);
    }
    let key =
        decode_secret_key(text, passphrase).map_err(|_| RemoteErrorCategory::KeyUndecodable)?;
    if key.algorithm().is_rsa() {
        return Err(RemoteErrorCategory::KeyNeedsAgent);
    }
    Ok(Arc::new(key))
}

struct RusshSftpConfig {
    route: SshRoute,
    root: String,
    login: SshLoginMaterial,
    host_key: HostKeyPolicy,
    known_hosts: PathBuf,
    agent_socket: Option<PathBuf>,
}

impl fmt::Debug for RusshSftpConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RusshSftpConfig")
            .field("host", &self.route.target.host)
            .field("port", &self.route.target.port)
            .field("jumps", &self.route.jumps.len())
            .field("root", &self.root)
            .field("host_key", &self.host_key)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct RusshSftpBuilder {
    service: Option<RusshSftpService>,
}

impl RusshSftpBuilder {
    fn new(service: RusshSftpService) -> Self {
        Self {
            service: Some(service),
        }
    }
}

impl Builder for RusshSftpBuilder {
    type Config = ();

    fn build(self) -> OpendalResult<impl Service> {
        self.service.ok_or_else(|| {
            Error::new(
                ErrorKind::ConfigInvalid,
                "russh SFTP service configuration is missing",
            )
        })
    }
}

struct RusshConnection {
    /// Every session on the way; the jump hosts carry the last one.
    _ssh: Vec<client::Handle<SftpHostKeyVerifier>>,
    sftp: Arc<RawSftpSession>,
}

impl fmt::Debug for RusshConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RusshConnection { .. }")
    }
}

struct RusshSftpInner {
    config: RusshSftpConfig,
    connection: Mutex<Option<Arc<RusshConnection>>>,
}

#[derive(Clone)]
struct RusshSftpService {
    inner: Arc<RusshSftpInner>,
}

impl fmt::Debug for RusshSftpService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RusshSftpService")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

impl RusshSftpService {
    fn new(config: RusshSftpConfig) -> Self {
        Self {
            inner: Arc::new(RusshSftpInner {
                config,
                connection: Mutex::new(None),
            }),
        }
    }

    async fn warm_up(&self) -> Result<(), RemoteErrorCategory> {
        self.connection().await.map(|_| ()).map_err(|error| {
            if error.kind() == ErrorKind::PermissionDenied {
                RemoteErrorCategory::Authentication
            } else if error.kind() == ErrorKind::NotFound {
                RemoteErrorCategory::UnknownHost
            } else if error.kind() == ErrorKind::ConditionNotMatch {
                RemoteErrorCategory::HostKey
            } else if error.is_temporary() {
                RemoteErrorCategory::Retryable
            } else {
                RemoteErrorCategory::Network
            }
        })
    }

    async fn connection(&self) -> OpendalResult<Arc<RusshConnection>> {
        let mut guard = self.inner.connection.lock().await;
        if let Some(connection) = guard.as_ref() {
            return Ok(connection.clone());
        }
        let connection = Arc::new(connect_sftp(&self.inner.config).await?);
        *guard = Some(connection.clone());
        Ok(connection)
    }

    async fn invalidate(&self, failed: &Arc<RusshConnection>) {
        let mut guard = self.inner.connection.lock().await;
        if guard
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, failed))
        {
            *guard = None;
        }
    }

    fn remote_path(&self, path: &str) -> String {
        let root = self.inner.config.root.trim_end_matches('/');
        let path = path.trim_matches('/');
        match (root.is_empty(), path.is_empty()) {
            (true, true) => "/".to_owned(),
            (true, false) => format!("/{path}"),
            (false, true) => root.to_owned(),
            (false, false) => format!("{root}/{path}"),
        }
    }

    async fn create_dir_all(&self, path: &str) -> OpendalResult<()> {
        let connection = self.connection().await?;
        let absolute = self.remote_path(path);
        let mut current = if absolute.starts_with('/') {
            "/".to_owned()
        } else {
            String::new()
        };
        for segment in absolute.trim_matches('/').split('/') {
            if segment.is_empty() {
                continue;
            }
            if current != "/" && !current.is_empty() {
                current.push('/');
            }
            current.push_str(segment);
            match connection
                .sftp
                .mkdir(current.clone(), FileAttributes::empty())
                .await
            {
                Ok(_) => {}
                Err(error) => match connection.sftp.stat(current.clone()).await {
                    Ok(attrs) if attrs.attrs.is_dir() => {}
                    _ => return Err(map_sftp_error(error)),
                },
            }
        }
        Ok(())
    }
}

impl Service for RusshSftpService {
    type Reader = RusshSftpReader;
    type Writer = RusshSftpWriter;
    type Lister = RusshSftpLister;
    type Deleter = oio::OneShotDeleter<RusshSftpDeleter>;
    type Copier = ();
    type Composer = ();

    fn info(&self) -> ServiceInfo {
        ServiceInfo::new(
            "sftp",
            &self.inner.config.root,
            &self.inner.config.route.target.host,
        )
    }

    fn capability(&self) -> Capability {
        Capability {
            stat: true,
            read: true,
            read_with_suffix: true,
            write: true,
            write_can_empty: true,
            write_can_append: true,
            write_with_if_not_exists: true,
            create_dir: true,
            delete: true,
            list: true,
            rename: true,
            ..Capability::default()
        }
    }

    async fn create_dir(
        &self,
        _ctx: &OperationContext,
        path: &str,
        _args: OpCreateDir,
    ) -> OpendalResult<RpCreateDir> {
        self.create_dir_all(path).await?;
        Ok(RpCreateDir::default())
    }

    async fn stat(
        &self,
        _ctx: &OperationContext,
        path: &str,
        _args: OpStat,
    ) -> OpendalResult<RpStat> {
        let remote = self.remote_path(path);
        for attempt in 0..2 {
            let connection = self.connection().await?;
            match connection.sftp.lstat(remote.clone()).await {
                Ok(attrs) => return Ok(RpStat::new(metadata_from_attrs(&attrs.attrs))),
                Err(error) if attempt == 0 && sftp_disconnected(&error) => {
                    self.invalidate(&connection).await;
                }
                Err(error) => return Err(map_sftp_error(error)),
            }
        }
        Err(temporary_sftp_error())
    }

    fn read(
        &self,
        _ctx: &OperationContext,
        path: &str,
        _args: OpRead,
    ) -> OpendalResult<Self::Reader> {
        Ok(RusshSftpReader {
            service: self.clone(),
            path: self.remote_path(path),
        })
    }

    fn write(
        &self,
        _ctx: &OperationContext,
        path: &str,
        args: OpWrite,
    ) -> OpendalResult<Self::Writer> {
        Ok(RusshSftpWriter {
            service: self.clone(),
            path: self.remote_path(path),
            args,
            connection: None,
            handle: None,
            offset: 0,
            closed: false,
        })
    }

    fn delete(&self, _ctx: &OperationContext) -> OpendalResult<Self::Deleter> {
        Ok(oio::OneShotDeleter::new(RusshSftpDeleter {
            service: self.clone(),
        }))
    }

    fn list(
        &self,
        _ctx: &OperationContext,
        path: &str,
        _args: OpList,
    ) -> OpendalResult<Self::Lister> {
        Ok(RusshSftpLister {
            service: self.clone(),
            path: path.to_owned(),
            remote_path: self.remote_path(path),
            connection: None,
            handle: None,
            entries: VecDeque::new(),
            finished: false,
        })
    }

    fn copy(
        &self,
        _ctx: &OperationContext,
        _from: &str,
        _to: &str,
        _args: OpCopy,
    ) -> OpendalResult<Self::Copier> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "russh SFTP does not support server-side copy",
        ))
    }

    async fn rename(
        &self,
        _ctx: &OperationContext,
        from: &str,
        to: &str,
        _args: OpRename,
    ) -> OpendalResult<RpRename> {
        let connection = self.connection().await?;
        connection
            .sftp
            .rename(self.remote_path(from), self.remote_path(to))
            .await
            .map_err(map_sftp_error)?;
        Ok(RpRename::default())
    }

    async fn presign(
        &self,
        _ctx: &OperationContext,
        _path: &str,
        _args: OpPresign,
    ) -> OpendalResult<RpPresign> {
        Err(Error::new(
            ErrorKind::Unsupported,
            "russh SFTP does not support presigned requests",
        ))
    }
}

struct RusshSftpReader {
    service: RusshSftpService,
    path: String,
}

impl oio::Read for RusshSftpReader {
    async fn open(
        &self,
        range: BytesRange,
    ) -> OpendalResult<(RpRead, Box<dyn oio::ReadStreamDyn>)> {
        let (reply, buffer) = self.read(range).await?;
        Ok((reply, Box::new(buffer)))
    }

    async fn read(&self, range: BytesRange) -> OpendalResult<(RpRead, Buffer)> {
        for attempt in 0..2 {
            let connection = self.service.connection().await?;
            match read_range(&connection.sftp, &self.path, range).await {
                Ok(buffer) => return Ok((RpRead::default(), Buffer::from(buffer))),
                Err(error) if attempt == 0 && sftp_disconnected(&error) => {
                    self.service.invalidate(&connection).await;
                }
                Err(error) => return Err(map_sftp_error(error)),
            }
        }
        Err(temporary_sftp_error())
    }
}

struct RusshSftpWriter {
    service: RusshSftpService,
    path: String,
    args: OpWrite,
    connection: Option<Arc<RusshConnection>>,
    handle: Option<String>,
    offset: u64,
    closed: bool,
}

impl RusshSftpWriter {
    async fn initialize(&mut self) -> OpendalResult<()> {
        if self.handle.is_some() {
            return Ok(());
        }
        let connection = self.service.connection().await?;
        let mut flags = OpenFlags::CREATE | OpenFlags::WRITE;
        if self.args.if_not_exists() {
            flags |= OpenFlags::EXCLUDE;
        } else if self.args.append() {
            flags |= OpenFlags::APPEND;
            self.offset = connection
                .sftp
                .lstat(self.path.clone())
                .await
                .map(|attrs| attrs.attrs.size.unwrap_or(0))
                .unwrap_or(0);
        } else {
            flags |= OpenFlags::TRUNCATE;
        }
        let handle = connection
            .sftp
            .open(self.path.clone(), flags, FileAttributes::empty())
            .await
            .map_err(map_sftp_error)?;
        self.handle = Some(handle.handle);
        self.connection = Some(connection);
        Ok(())
    }
}

impl oio::Write for RusshSftpWriter {
    async fn write(&mut self, buffer: Buffer) -> OpendalResult<()> {
        self.initialize().await?;
        let connection = self.connection.as_ref().expect("writer is initialized");
        let handle = self.handle.as_ref().expect("writer is initialized");
        let bytes = buffer.to_bytes();
        for chunk in bytes.chunks(SFTP_CHUNK_SIZE) {
            connection
                .sftp
                .write(handle.clone(), self.offset, chunk.to_vec())
                .await
                .map_err(map_sftp_error)?;
            self.offset = self.offset.saturating_add(chunk.len() as u64);
        }
        Ok(())
    }

    async fn close(&mut self) -> OpendalResult<Metadata> {
        self.initialize().await?;
        let connection = self.connection.as_ref().expect("writer is initialized");
        let handle = self.handle.take().expect("writer is initialized");
        connection
            .sftp
            .close(handle)
            .await
            .map_err(map_sftp_error)?;
        self.closed = true;
        let attrs = connection
            .sftp
            .lstat(self.path.clone())
            .await
            .map_err(map_sftp_error)?;
        Ok(metadata_from_attrs(&attrs.attrs))
    }

    async fn abort(&mut self) -> OpendalResult<()> {
        if let (Some(connection), Some(handle)) = (&self.connection, self.handle.take()) {
            connection
                .sftp
                .close(handle)
                .await
                .map_err(map_sftp_error)?;
            match connection.sftp.remove(self.path.clone()).await {
                Ok(_) => {}
                Err(error) if sftp_not_found(&error) => {}
                Err(error) => return Err(map_sftp_error(error)),
            }
        }
        self.closed = true;
        Ok(())
    }
}

impl Drop for RusshSftpWriter {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let (Some(connection), Some(handle)) = (self.connection.take(), self.handle.take()) else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = connection.sftp.close(handle).await;
            });
        }
    }
}

struct RusshSftpLister {
    service: RusshSftpService,
    path: String,
    remote_path: String,
    connection: Option<Arc<RusshConnection>>,
    handle: Option<String>,
    entries: VecDeque<File>,
    finished: bool,
}

impl RusshSftpLister {
    async fn initialize(&mut self) -> OpendalResult<()> {
        if self.handle.is_some() || self.finished {
            return Ok(());
        }
        let connection = self.service.connection().await?;
        match connection.sftp.opendir(self.remote_path.clone()).await {
            Ok(handle) => {
                self.handle = Some(handle.handle);
                self.connection = Some(connection);
                Ok(())
            }
            Err(error) if sftp_not_found(&error) => {
                self.finished = true;
                Ok(())
            }
            Err(error) => Err(map_sftp_error(error)),
        }
    }

    async fn finish(&mut self) -> OpendalResult<()> {
        self.finished = true;
        if let (Some(connection), Some(handle)) = (&self.connection, self.handle.take()) {
            connection
                .sftp
                .close(handle)
                .await
                .map_err(map_sftp_error)?;
        }
        Ok(())
    }

    fn entry(&self, file: File) -> Option<oio::Entry> {
        if matches!(file.filename.as_str(), "." | "..") {
            return None;
        }
        let metadata = metadata_from_attrs(&file.attrs);
        let mut path = if self.path == "/" || self.path.is_empty() {
            file.filename
        } else {
            format!("{}/{}", self.path.trim_end_matches('/'), file.filename)
        };
        if metadata.is_dir() && !path.ends_with('/') {
            path.push('/');
        }
        Some(oio::Entry::new(&path, metadata))
    }
}

impl oio::List for RusshSftpLister {
    async fn next(&mut self) -> OpendalResult<Option<oio::Entry>> {
        loop {
            if let Some(file) = self.entries.pop_front() {
                if let Some(entry) = self.entry(file) {
                    return Ok(Some(entry));
                }
                continue;
            }
            self.initialize().await?;
            if self.finished {
                return Ok(None);
            }
            let connection = self.connection.as_ref().expect("lister is initialized");
            let handle = self.handle.as_ref().expect("lister is initialized");
            match connection.sftp.readdir(handle.clone()).await {
                Ok(page) => self.entries.extend(page.files),
                Err(SftpError::Status(status)) if status.status_code == StatusCode::Eof => {
                    self.finish().await?;
                    return Ok(None);
                }
                Err(error) => {
                    if sftp_disconnected(&error) {
                        let failed = connection.clone();
                        self.service.invalidate(&failed).await;
                    }
                    return Err(map_sftp_error(error));
                }
            }
        }
    }
}

impl Drop for RusshSftpLister {
    fn drop(&mut self) {
        let (Some(connection), Some(handle)) = (self.connection.take(), self.handle.take()) else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = connection.sftp.close(handle).await;
            });
        }
    }
}

struct RusshSftpDeleter {
    service: RusshSftpService,
}

impl oio::OneShotDelete for RusshSftpDeleter {
    async fn delete_once(&self, path: String, _args: OpDelete) -> OpendalResult<()> {
        let connection = self.service.connection().await?;
        let remote = self.service.remote_path(&path);
        let result = if path.ends_with('/') {
            connection.sftp.rmdir(remote).await
        } else {
            connection.sftp.remove(remote).await
        };
        match result {
            Ok(_) => Ok(()),
            Err(error) if sftp_not_found(&error) => Ok(()),
            Err(error) => Err(map_sftp_error(error)),
        }
    }
}

#[derive(Clone)]
struct SftpHostKeyVerifier {
    host: String,
    port: u16,
    policy: HostKeyPolicy,
    known_hosts: PathBuf,
}

impl client::Handler for SftpHostKeyVerifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let public_key = server_public_key.public_key();
        // `false` means known_hosts has no key for the host; a key that
        // differs from the recorded or pinned one fails as a changed key.
        match &self.policy {
            HostKeyPolicy::KnownHosts => match russh::keys::check_known_hosts_path(
                &self.host,
                self.port,
                &public_key,
                &self.known_hosts,
            ) {
                Ok(known) => Ok(known),
                Err(russh::keys::Error::KeyChanged { line }) => {
                    Err(russh::Error::KeyChanged { line })
                }
                Err(_) => Ok(false),
            },
            HostKeyPolicy::PinnedSha256(expected) => {
                if Sha256::digest(public_key.public_key_bytes()).as_slice() == expected {
                    Ok(true)
                } else {
                    Err(russh::Error::KeyChanged { line: 0 })
                }
            }
        }
    }
}

async fn connect_sftp(config: &RusshSftpConfig) -> OpendalResult<RusshConnection> {
    let client_config = Arc::new(client::Config::default());
    let mut sessions: Vec<client::Handle<SftpHostKeyVerifier>> = Vec::new();
    let last = config.route.jumps.len();
    let hops = config
        .route
        .jumps
        .iter()
        .chain(std::iter::once(&config.route.target));
    for (index, hop) in hops.enumerate() {
        let target = index == last;
        let verifier = SftpHostKeyVerifier {
            host: hop.host.clone(),
            port: hop.port,
            // A jump host must match known_hosts; only the connection's own
            // server may be pinned.
            policy: if target {
                config.host_key.clone()
            } else {
                HostKeyPolicy::KnownHosts
            },
            known_hosts: config.known_hosts.clone(),
        };
        let connect = async {
            match sessions.last() {
                None => {
                    client::connect(
                        Arc::clone(&client_config),
                        (hop.host.as_str(), hop.port),
                        verifier,
                    )
                    .await
                }
                Some(previous) => {
                    let channel = previous
                        .channel_open_direct_tcpip(
                            hop.host.clone(),
                            u32::from(hop.port),
                            "127.0.0.1",
                            0,
                        )
                        .await?;
                    client::connect_stream(
                        Arc::clone(&client_config),
                        channel.into_stream(),
                        verifier,
                    )
                    .await
                }
            }
        };
        let mut ssh = tokio::time::timeout(Duration::from_secs(10), connect)
            .await
            .map_err(|_| temporary_sftp_error())?
            .map_err(|error| map_ssh_error(&error))?;
        if !authenticate(&mut ssh, hop, target, config).await? {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                "SFTP authentication failed",
            ));
        }
        sessions.push(ssh);
    }
    let ssh = sessions
        .last()
        .ok_or_else(|| Error::new(ErrorKind::Unexpected, "SFTP route has no server"))?;
    let channel = ssh
        .channel_open_session()
        .await
        .map_err(|error| map_ssh_error(&error))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|error| map_ssh_error(&error))?;
    let raw = Arc::new(RawSftpSession::new(channel.into_stream()));
    raw.init().await.map_err(map_sftp_error)?;
    Ok(RusshConnection {
        _ssh: sessions,
        sftp: raw,
    })
}

/// Logs in to one hop. The connection's server takes the profile's login; a
/// jump host takes the agent's keys, the connection's own key, and the keys
/// its Host entry names.
async fn authenticate(
    ssh: &mut client::Handle<SftpHostKeyVerifier>,
    hop: &SshHop,
    target: bool,
    config: &RusshSftpConfig,
) -> OpendalResult<bool> {
    let denied = |_| Error::new(ErrorKind::PermissionDenied, "SFTP authentication failed");
    if (!target || matches!(config.login, SshLoginMaterial::Agent))
        && let Some(socket) = &config.agent_socket
        && login_with_agent(ssh, &hop.user, socket).await
    {
        return Ok(true);
    }
    match (&config.login, target) {
        (SshLoginMaterial::Password(Some(secret)), true) => {
            return ssh
                .authenticate_password(hop.user.clone(), secret.to_string())
                .await
                .map(|result| result.success())
                .map_err(denied);
        }
        (SshLoginMaterial::Password(None), true) => {
            return ssh
                .authenticate_none(hop.user.clone())
                .await
                .map(|result| result.success())
                .map_err(denied);
        }
        (SshLoginMaterial::Key(key), _) => {
            let key = PrivateKeyWithHashAlg::new(Arc::clone(key), None);
            if ssh
                .authenticate_publickey(hop.user.clone(), key)
                .await
                .map_err(denied)?
                .success()
            {
                return Ok(true);
            }
        }
        _ => {}
    }
    if !target {
        for path in &hop.identity_files {
            let Ok(key) = russh::keys::load_secret_key(path, None) else {
                continue;
            };
            if key.algorithm().is_rsa() {
                continue;
            }
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), None);
            if ssh
                .authenticate_publickey(hop.user.clone(), key)
                .await
                .map_err(denied)?
                .success()
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Offers each key the agent at `socket` holds. The agent signs an RSA key
/// with SHA-2, never SHA-1.
async fn login_with_agent(
    ssh: &mut client::Handle<SftpHostKeyVerifier>,
    user: &str,
    socket: &std::path::Path,
) -> bool {
    let Ok(stream) = tokio::net::UnixStream::connect(socket).await else {
        return false;
    };
    let mut agent = russh::keys::agent::client::AgentClient::connect(stream);
    let Ok(identities) = agent.request_identities().await else {
        return false;
    };
    for identity in identities {
        let rsa = match &identity {
            russh::keys::agent::AgentIdentity::PublicKey { key, .. } => key.algorithm().is_rsa(),
            russh::keys::agent::AgentIdentity::Certificate { certificate, .. } => {
                certificate.algorithm().is_rsa()
            }
        };
        let hash = if rsa {
            Some(match ssh.best_supported_rsa_hash().await {
                Ok(Some(Some(HashAlg::Sha512))) => HashAlg::Sha512,
                _ => HashAlg::Sha256,
            })
        } else {
            None
        };
        let accepted = match identity {
            russh::keys::agent::AgentIdentity::PublicKey { key, .. } => ssh
                .authenticate_publickey_with(user.to_owned(), key, hash, &mut agent)
                .await
                .is_ok_and(|result| result.success()),
            russh::keys::agent::AgentIdentity::Certificate { certificate, .. } => ssh
                .authenticate_certificate_with(user.to_owned(), certificate, hash, &mut agent)
                .await
                .is_ok_and(|result| result.success()),
        };
        if accepted {
            return true;
        }
    }
    false
}

async fn read_range(
    session: &RawSftpSession,
    path: &str,
    range: BytesRange,
) -> Result<Vec<u8>, SftpError> {
    let attrs = session.lstat(path.to_owned()).await?;
    let length = attrs.attrs.size.unwrap_or(0);
    let (mut offset, size) = if range.is_suffix() {
        let size = range.size().unwrap_or(0).min(length);
        (length.saturating_sub(size), Some(size))
    } else {
        (range.offset(), range.size())
    };
    let handle = session
        .open(path.to_owned(), OpenFlags::READ, FileAttributes::empty())
        .await?;
    let mut remaining = size.unwrap_or_else(|| length.saturating_sub(offset));
    let mut output = Vec::with_capacity(usize::try_from(remaining).unwrap_or(0));
    let result = async {
        while remaining > 0 {
            let request = remaining.min(SFTP_CHUNK_SIZE as u64) as u32;
            match session.read(handle.handle.clone(), offset, request).await {
                Ok(data) if data.data.is_empty() => break,
                Ok(data) => {
                    offset = offset.saturating_add(data.data.len() as u64);
                    remaining = remaining.saturating_sub(data.data.len() as u64);
                    output.extend_from_slice(&data.data);
                }
                Err(SftpError::Status(status)) if status.status_code == StatusCode::Eof => break,
                Err(error) => return Err(error),
            }
        }
        Ok(output)
    }
    .await;
    let close = session.close(handle.handle).await;
    match (result, close) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(output), Ok(_)) => Ok(output),
    }
}

fn metadata_from_attrs(attrs: &FileAttributes) -> Metadata {
    let mut builder = if attrs.is_dir() {
        MetadataBuilder::dir()
    } else if attrs.is_regular() {
        match attrs.size {
            Some(size) => MetadataBuilder::file(size),
            None => MetadataBuilder::unknown(),
        }
    } else {
        MetadataBuilder::unknown()
    };
    if let Some(modified) = attrs.mtime
        && let Ok(timestamp) = Timestamp::new(i64::from(modified), 0)
    {
        builder.last_modified(timestamp);
    }
    builder.build()
}

fn sftp_not_found(error: &SftpError) -> bool {
    matches!(
        error,
        SftpError::Status(status) if status.status_code == StatusCode::NoSuchFile
    )
}

fn sftp_disconnected(error: &SftpError) -> bool {
    match error {
        SftpError::IO(_) | SftpError::Timeout => true,
        SftpError::UnexpectedBehavior(message) => unexpected_disconnect(message),
        SftpError::Status(status) => matches!(
            status.status_code,
            StatusCode::NoConnection | StatusCode::ConnectionLost
        ),
        _ => false,
    }
}

fn map_sftp_error(error: SftpError) -> Error {
    let (kind, temporary) = match &error {
        SftpError::Status(status) => match status.status_code {
            StatusCode::NoSuchFile => (ErrorKind::NotFound, false),
            StatusCode::PermissionDenied => (ErrorKind::PermissionDenied, false),
            StatusCode::OpUnsupported => (ErrorKind::Unsupported, false),
            StatusCode::NoConnection | StatusCode::ConnectionLost => (ErrorKind::Unexpected, true),
            _ => (ErrorKind::Unexpected, false),
        },
        SftpError::IO(_) | SftpError::Timeout => (ErrorKind::Unexpected, true),
        SftpError::Limited(_) => (ErrorKind::RateLimited, true),
        SftpError::UnexpectedBehavior(message) if unexpected_disconnect(message) => {
            (ErrorKind::Unexpected, true)
        }
        SftpError::UnexpectedPacket | SftpError::UnexpectedBehavior(_) => {
            (ErrorKind::Unexpected, false)
        }
    };
    let mapped = Error::new(kind, "SFTP operation failed");
    if temporary {
        mapped.set_temporary()
    } else {
        mapped
    }
}

fn unexpected_disconnect(message: &str) -> bool {
    message == "session closed"
        || message.starts_with("SendError:")
        || message.starts_with("RecvError:")
}

fn map_ssh_error(error: &russh::Error) -> Error {
    match error {
        russh::Error::UnknownKey => {
            Error::new(ErrorKind::NotFound, "SFTP host key is not in known_hosts")
        }
        russh::Error::KeyChanged { .. } => Error::new(
            ErrorKind::ConditionNotMatch,
            "SFTP host key verification failed",
        ),
        _ => temporary_sftp_error(),
    }
}

fn temporary_sftp_error() -> Error {
    Error::new(ErrorKind::Unexpected, "SFTP transport failed").set_temporary()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::sftp_test_server::{
        Accepts, CertificateAgent, RecordingRsaAgent, TestAgent, TestSshServer, ed25519_key,
        fabricated_rsa_key, host_key_pin, known_hosts_line, user_certificate, write_key_file,
    };
    use crate::{ConnectionId, CredentialReference, RemoteHost, SecretBuffer, SshLogin};
    use futures_lite::future::block_on;
    use musheen_core::{PageRequest, Store};
    use russh::keys::ssh_key::LineEnding;
    use russh::keys::{PrivateKey, PublicKeyBase64};
    use std::collections::HashMap;
    use std::path::Path;

    /// Secrets by connection ID, standing in for the secret service.
    #[derive(Default)]
    struct Secrets(HashMap<String, Vec<u8>>);

    impl Secrets {
        fn with(mut self, id: &str, secret: &[u8]) -> Self {
            self.0.insert(id.to_owned(), secret.to_vec());
            self
        }
    }

    impl CredentialResolver for Secrets {
        fn resolve<'a>(
            &'a self,
            reference: &'a CredentialReference,
            _cancellation: CancellationToken,
        ) -> BoxFuture<'a, Result<SecretBuffer, RemoteErrorCategory>> {
            let secret = self
                .0
                .get(reference.connection_id().as_str())
                .cloned()
                .map(SecretBuffer::new)
                .ok_or(RemoteErrorCategory::Authentication);
            Box::pin(async move { secret })
        }
    }

    fn environment(directory: &Path) -> SshEnvironment {
        SshEnvironment {
            home: directory.to_path_buf(),
            config: None,
            known_hosts: directory.join("known_hosts"),
            agent_socket: None,
        }
    }

    /// An SFTP profile for `host`, pinned to the test server's host key.
    fn profile(
        host: &str,
        port: Option<u16>,
        user: Option<&str>,
        host_key: &PrivateKey,
        credential: bool,
        login: SshLogin,
    ) -> ConnectionProfile {
        ConnectionProfile::new(
            ConnectionId::new("office").unwrap(),
            "Office",
            RemoteProtocol::Sftp,
            RemoteHost::new(RemoteProtocol::Sftp, host).unwrap(),
            port,
            "/",
            user,
            credential
                .then(|| CredentialReference::persistent(ConnectionId::new("office").unwrap())),
            SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(host_key_pin(host_key))),
            None,
        )
        .unwrap()
        .with_login(login)
        .unwrap()
    }

    fn open(
        profile: &ConnectionProfile,
        secrets: &Secrets,
        environment: &SshEnvironment,
    ) -> Result<OpendalStore, RemoteError> {
        block_on(sftp_store_from_profile_in(
            ProviderId::new("sftp-login-test").unwrap(),
            profile,
            secrets,
            environment,
            CancellationToken::new(),
        ))
    }

    fn lists_root(store: &OpendalStore) -> Vec<String> {
        block_on(store.read_directory(
            &store.root_path(),
            PageRequest::new(16, None).unwrap(),
            CancellationToken::new(),
        ))
        .expect("the root lists")
        .items()
        .iter()
        .map(|item| item.display_name().as_str().to_owned())
        .collect()
    }

    fn accepts(user: &str, keys: &[&PrivateKey]) -> Accepts {
        Accepts {
            user: user.to_owned(),
            keys: keys.iter().map(|key| key.public_key().clone()).collect(),
            ..Accepts::default()
        }
    }

    fn key_login(user: &str, key: &PrivateKey) -> String {
        format!("{user} key {}", key.public_key().public_key_base64())
    }

    #[test]
    fn sftp_login_with_a_password_offers_it() {
        let server = TestSshServer::start(
            ed25519_key(1),
            Accepts {
                user: "alice".into(),
                password: Some("open sesame".into()),
                ..Accepts::default()
            },
        );
        let scratch = tempfile::tempdir().unwrap();
        let profile = profile(
            "127.0.0.1",
            Some(server.port()),
            Some("alice"),
            server.host_key(),
            true,
            SshLogin::Password,
        );

        let store = open(
            &profile,
            &Secrets::default().with("office", b"open sesame"),
            &environment(scratch.path()),
        )
        .expect("a password the server accepts logs in");

        assert_eq!(lists_root(&store), ["hello.txt"]);
        assert_eq!(server.log().logins(), ["alice password"]);
    }

    #[test]
    fn sftp_login_with_the_agent_offers_the_agent_keys() {
        let client_key = ed25519_key(7);
        let server = TestSshServer::start(ed25519_key(2), accepts("alice", &[&client_key]));
        let scratch = tempfile::tempdir().unwrap();
        let agent = TestAgent::start(scratch.path(), std::slice::from_ref(&client_key));
        let mut environment = environment(scratch.path());
        environment.agent_socket = Some(agent.socket().to_path_buf());
        let profile = profile(
            "127.0.0.1",
            Some(server.port()),
            Some("alice"),
            server.host_key(),
            false,
            SshLogin::Agent,
        );

        let store = open(&profile, &Secrets::default(), &environment)
            .expect("a key the agent holds logs in");

        assert_eq!(lists_root(&store), ["hello.txt"]);
        assert_eq!(server.log().logins(), [key_login("alice", &client_key)]);
    }

    #[test]
    fn sftp_login_with_the_agent_offers_its_certificates() {
        let authority = ed25519_key(60);
        let client_key = ed25519_key(61);
        let certificate = user_certificate(&authority, &client_key, "alice");
        let server = TestSshServer::start(
            ed25519_key(62),
            Accepts {
                user: "alice".into(),
                certificate_authority: Some(authority.public_key().clone()),
                ..Accepts::default()
            },
        );
        let scratch = tempfile::tempdir().unwrap();
        let agent = CertificateAgent::start(scratch.path(), client_key, &certificate);
        let mut environment = environment(scratch.path());
        environment.agent_socket = Some(agent.socket().to_path_buf());
        let profile = profile(
            "127.0.0.1",
            Some(server.port()),
            Some("alice"),
            server.host_key(),
            false,
            SshLogin::Agent,
        );

        let store = open(&profile, &Secrets::default(), &environment)
            .expect("a certificate the agent holds logs in");

        assert_eq!(lists_root(&store), ["hello.txt"]);
        assert_eq!(server.log().logins(), ["alice certificate"]);
    }

    #[test]
    fn sftp_login_names_the_local_cause_of_each_failure() {
        let server = TestSshServer::start(ed25519_key(63), accepts("alice", &[]));
        let scratch = tempfile::tempdir().unwrap();
        let cause =
            |profile: &ConnectionProfile, secrets: &Secrets, environment: &SshEnvironment| {
                open(profile, secrets, environment)
                    .err()
                    .map(|error| error.category())
            };
        let login = |login: SshLogin, credential: bool| {
            profile(
                "127.0.0.1",
                Some(server.port()),
                Some("alice"),
                server.host_key(),
                credential,
                login,
            )
        };

        let mut no_agent = environment(scratch.path());
        assert_eq!(
            cause(
                &login(SshLogin::Agent, false),
                &Secrets::default(),
                &no_agent
            ),
            Some(RemoteErrorCategory::NoAgent),
            "agent login with no agent names the agent"
        );
        no_agent.agent_socket = Some(scratch.path().join("gone.sock"));
        assert_eq!(
            cause(
                &login(SshLogin::Agent, false),
                &Secrets::default(),
                &no_agent
            ),
            Some(RemoteErrorCategory::NoAgent),
            "an agent socket nothing answers names the agent"
        );

        let missing = SshLogin::KeyFile {
            path: Some(scratch.path().join("missing").to_string_lossy().into()),
        };
        assert_eq!(
            cause(
                &login(missing, false),
                &Secrets::default(),
                &environment(scratch.path())
            ),
            Some(RemoteErrorCategory::KeyUnreadable)
        );

        let key_path = scratch.path().join("id_office");
        write_key_file(&key_path, &ed25519_key(64), Some("right"));
        let encrypted = SshLogin::KeyFile {
            path: Some(key_path.to_string_lossy().into()),
        };
        assert_eq!(
            cause(
                &login(encrypted, true),
                &Secrets::default().with("office", b"wrong"),
                &environment(scratch.path())
            ),
            Some(RemoteErrorCategory::KeyUndecodable),
            "a wrong passphrase names the key, not the server"
        );

        let config = scratch.path().join("config");
        let mut with_config = environment(scratch.path());
        with_config.config = Some(config.clone());
        std::fs::write(&config, "Host 127.0.0.1\n  Port not-a-port\n").unwrap();
        assert_eq!(
            cause(
                &login(SshLogin::Password, false),
                &Secrets::default(),
                &with_config
            ),
            Some(RemoteErrorCategory::SshConfigUnreadable)
        );
        std::fs::write(
            &config,
            "Host 127.0.0.1\n  User alice\nMatch host *.example.com\n  User bob\n",
        )
        .unwrap();
        assert_eq!(
            cause(
                &login(SshLogin::Password, false),
                &Secrets::default(),
                &with_config
            ),
            Some(RemoteErrorCategory::SshConfigMatch),
            "options a Match block would put on the Host entry are refused, not used"
        );

        let unknown = ConnectionProfile::new(
            ConnectionId::new("office").unwrap(),
            "Office",
            RemoteProtocol::Sftp,
            RemoteHost::new(RemoteProtocol::Sftp, "127.0.0.1").unwrap(),
            Some(server.port()),
            "/",
            Some("alice"),
            None,
            SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
            None,
        )
        .unwrap();
        assert_eq!(
            cause(&unknown, &Secrets::default(), &environment(scratch.path())),
            Some(RemoteErrorCategory::UnknownHost),
            "a host missing from known_hosts is not a changed key"
        );
    }

    #[test]
    fn sftp_login_expands_tokens_and_reaches_the_first_jump_host_through_its_own_proxy_jump() {
        let scratch = tempfile::tempdir().unwrap();
        let config = scratch.path().join("config");
        std::fs::write(
            &config,
            "Host office\n  HostName %h.inside\n  User alice\n  IdentityFile %d/keys/%r@%h\n  ProxyJump gate,ssh://carol@[::1]:2203\nHost gate\n  HostName gate.example\n  Port 2201\n  ProxyJump bob@outer:2202\n",
        )
        .unwrap();
        let mut environment = environment(scratch.path());
        environment.config = Some(config);
        let profile = profile(
            "office",
            None,
            None,
            &ed25519_key(65),
            false,
            SshLogin::Password,
        );

        let route = SshRoute::resolve(&profile, &environment).unwrap();

        assert_eq!(route.target.host, "office.inside");
        assert_eq!(
            route.target.identity_files,
            [scratch.path().join("keys/alice@office.inside")]
        );
        let hops = route
            .jumps
            .iter()
            .map(|hop| (hop.user.as_str(), hop.host.as_str(), hop.port))
            .collect::<Vec<_>>();
        assert_eq!(
            hops,
            [
                ("bob", "outer", 2202),
                ("alice", "gate.example", 2201),
                ("carol", "::1", 2203),
            ]
        );
    }

    #[test]
    fn sftp_login_reads_proxy_jump_addresses() {
        assert_eq!(
            parse_jump("ssh://bob@host:2222"),
            Ok((Some("bob".into()), "host".into(), Some(2222)))
        );
        assert_eq!(
            parse_jump("[::1]:2200"),
            Ok((None, "::1".into(), Some(2200)))
        );
        assert_eq!(parse_jump("fe80::1"), Ok((None, "fe80::1".into(), None)));
        assert_eq!(
            parse_jump("alice@[fe80::1]"),
            Ok((Some("alice".into()), "fe80::1".into(), None))
        );
        assert_eq!(
            parse_jump("host:0"),
            Err(RemoteErrorCategory::SshConfigUnreadable)
        );
    }

    #[test]
    fn sftp_login_with_an_encrypted_key_file_uses_its_stored_passphrase() {
        let client_key = ed25519_key(8);
        let server = TestSshServer::start(ed25519_key(3), accepts("alice", &[&client_key]));
        let scratch = tempfile::tempdir().unwrap();
        let key_path = scratch.path().join("id_office");
        write_key_file(&key_path, &client_key, Some("correct horse"));
        let profile = profile(
            "127.0.0.1",
            Some(server.port()),
            Some("alice"),
            server.host_key(),
            true,
            SshLogin::KeyFile {
                path: Some(key_path.to_str().unwrap().into()),
            },
        );

        let store = open(
            &profile,
            &Secrets::default().with("office", b"correct horse"),
            &environment(scratch.path()),
        )
        .expect("an encrypted key file and its passphrase log in");

        assert_eq!(lists_root(&store), ["hello.txt"]);
        assert_eq!(server.log().logins(), [key_login("alice", &client_key)]);
    }

    #[test]
    fn sftp_login_with_a_stored_key_reads_the_key_and_its_passphrase() {
        let client_key = ed25519_key(9);
        let server = TestSshServer::start(ed25519_key(4), accepts("alice", &[&client_key]));
        let scratch = tempfile::tempdir().unwrap();
        let encrypted = client_key
            .encrypt(&mut rand::rng(), "stored passphrase")
            .unwrap()
            .to_openssh(LineEnding::LF)
            .unwrap();
        let profile = profile(
            "127.0.0.1",
            Some(server.port()),
            Some("alice"),
            server.host_key(),
            true,
            SshLogin::StoredKey,
        );
        let key_reference = profile.stored_key_reference().unwrap();
        let secrets = Secrets::default()
            .with("office", b"stored passphrase")
            .with(key_reference.connection_id().as_str(), encrypted.as_bytes());

        let store = open(&profile, &secrets, &environment(scratch.path()))
            .expect("a stored key and its passphrase log in");

        assert_eq!(lists_root(&store), ["hello.txt"]);
        assert_eq!(server.log().logins(), [key_login("alice", &client_key)]);
    }

    #[test]
    fn sftp_login_refuses_an_rsa_key_file_and_points_to_the_agent() {
        let server = TestSshServer::start(ed25519_key(5), accepts("alice", &[]));
        let scratch = tempfile::tempdir().unwrap();
        let key_path = scratch.path().join("id_rsa_test");
        write_key_file(&key_path, &fabricated_rsa_key(), None);
        let profile = profile(
            "127.0.0.1",
            Some(server.port()),
            Some("alice"),
            server.host_key(),
            false,
            SshLogin::KeyFile {
                path: Some(key_path.to_str().unwrap().into()),
            },
        );

        let error = open(&profile, &Secrets::default(), &environment(scratch.path()))
            .err()
            .expect("Musheen signs with no RSA key file");

        assert_eq!(error.category(), RemoteErrorCategory::KeyNeedsAgent);
    }

    #[test]
    fn sftp_login_asks_the_agent_for_a_sha2_rsa_signature() {
        let server = TestSshServer::start(
            ed25519_key(6),
            Accepts {
                user: "alice".into(),
                keys: vec![RecordingRsaAgent::public_key()],
                ..Accepts::default()
            },
        );
        let scratch = tempfile::tempdir().unwrap();
        let agent = RecordingRsaAgent::start(scratch.path());
        let mut environment = environment(scratch.path());
        environment.agent_socket = Some(agent.socket().to_path_buf());
        let profile = profile(
            "127.0.0.1",
            Some(server.port()),
            Some("alice"),
            server.host_key(),
            false,
            SshLogin::Agent,
        );

        let _ = open(&profile, &Secrets::default(), &environment);

        let flags = agent.sign_flags();
        assert!(
            !flags.is_empty(),
            "the login asks the agent to sign with its RSA key"
        );
        assert!(
            flags.iter().all(|flag| matches!(flag, 2 | 4)),
            "every RSA signature request asks for SHA-2, never SHA-1: {flags:?}"
        );
    }

    #[test]
    fn sftp_login_applies_the_host_entry_of_an_included_ssh_config() {
        let client_key = ed25519_key(10);
        let server = TestSshServer::start(ed25519_key(11), accepts("alice", &[&client_key]));
        let scratch = tempfile::tempdir().unwrap();
        let key_path = scratch.path().join("office_key");
        write_key_file(&key_path, &client_key, None);
        let included = scratch.path().join("config.d");
        std::fs::create_dir(&included).unwrap();
        std::fs::write(
            included.join("office.conf"),
            format!(
                "Host office\n  HostName 127.0.0.1\n  Port {}\n  User alice\n  IdentityFile {}\n",
                server.port(),
                key_path.display()
            ),
        )
        .unwrap();
        let config = scratch.path().join("config");
        std::fs::write(&config, format!("Include {}/*.conf\n", included.display())).unwrap();
        let mut environment = environment(scratch.path());
        environment.config = Some(config);
        let profile = profile(
            "office",
            None,
            None,
            server.host_key(),
            false,
            SshLogin::KeyFile { path: None },
        );

        let store = open(&profile, &Secrets::default(), &environment)
            .expect("the Host entry supplies the address, user and key");

        assert_eq!(lists_root(&store), ["hello.txt"]);
        assert_eq!(server.log().logins(), [key_login("alice", &client_key)]);
    }

    fn jump_setup(
        trust_jump_host: bool,
    ) -> (
        TestSshServer,
        TestSshServer,
        tempfile::TempDir,
        TestAgent,
        SshEnvironment,
        ConnectionProfile,
        PrivateKey,
    ) {
        let client_key = ed25519_key(12);
        let target = TestSshServer::start(ed25519_key(13), accepts("alice", &[&client_key]));
        let mut jump_accepts = accepts("alice", &[&client_key]);
        jump_accepts.forwarding = true;
        let jump = TestSshServer::start(ed25519_key(14), jump_accepts);
        let scratch = tempfile::tempdir().unwrap();
        let agent = TestAgent::start(scratch.path(), std::slice::from_ref(&client_key));
        let config = scratch.path().join("config");
        std::fs::write(
            &config,
            format!(
                "Host inside\n  HostName 127.0.0.1\n  Port {}\n  User alice\n  ProxyJump gateway\n\nHost gateway\n  HostName 127.0.0.1\n  Port {}\n  User alice\n",
                target.port(),
                jump.port()
            ),
        )
        .unwrap();
        let trusted = if trust_jump_host {
            jump.host_key().clone()
        } else {
            ed25519_key(15)
        };
        std::fs::write(
            scratch.path().join("known_hosts"),
            known_hosts_line("127.0.0.1", jump.port(), &trusted),
        )
        .unwrap();
        let mut environment = environment(scratch.path());
        environment.config = Some(config);
        environment.agent_socket = Some(agent.socket().to_path_buf());
        let profile = profile(
            "inside",
            None,
            None,
            target.host_key(),
            false,
            SshLogin::Agent,
        );
        (
            target,
            jump,
            scratch,
            agent,
            environment,
            profile,
            client_key,
        )
    }

    #[test]
    fn sftp_login_goes_through_the_jump_host_of_the_host_entry() {
        let (target, jump, _scratch, _agent, environment, profile, client_key) = jump_setup(true);

        let store = open(&profile, &Secrets::default(), &environment)
            .expect("the login reaches the server through its jump host");

        assert_eq!(lists_root(&store), ["hello.txt"]);
        assert_eq!(
            jump.log().forwards(),
            [("127.0.0.1".to_owned(), u32::from(target.port()))]
        );
        assert_eq!(target.log().logins(), [key_login("alice", &client_key)]);
    }

    #[test]
    fn sftp_login_refuses_a_jump_host_whose_key_known_hosts_does_not_match() {
        let (target, jump, _scratch, _agent, environment, profile, _) = jump_setup(false);

        assert!(open(&profile, &Secrets::default(), &environment).is_err());
        assert!(
            jump.log().forwards().is_empty(),
            "nothing is forwarded through an untrusted jump host"
        );
        assert!(target.log().logins().is_empty());
    }

    #[test]
    fn closed_raw_session_is_reconnectable_and_retryable() {
        for message in [
            "SendError: channel closed",
            "RecvError: channel closed",
            "session closed",
        ] {
            let error = SftpError::UnexpectedBehavior(message.to_owned());
            assert!(sftp_disconnected(&error));
            assert!(map_sftp_error(error).is_temporary());
        }
    }
}
