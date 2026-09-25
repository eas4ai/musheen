use super::{
    ConnectionProfile, CredentialResolver, HostKeyPolicy, OpendalStore, RemoteCasePolicy,
    RemoteError, RemoteErrorCategory, RemoteMutationPolicy, RemoteProtocol, SecurityPolicy,
};
use musheen_core::{BoxFuture, CancellationToken, ProviderId};
use opendal::raw::{
    OpCopy, OpCreateDir, OpDelete, OpList, OpPresign, OpRead, OpRename, OpStat, OpWrite,
    RpCreateDir, RpPresign, RpRead, RpRename, RpStat, Service, ServiceInfo, Timestamp, oio,
};
use opendal::{
    Buffer, Builder, BytesRange, Capability, Error, ErrorKind, Metadata, MetadataBuilder,
    OperationContext, Operator, Result as OpendalResult, services::Sftp,
};
use russh::client;
use russh::keys::{
    PrivateKeyWithHashAlg, PublicKeyBase64, PublicKeyOrCertificate, decode_secret_key,
};
use russh_sftp::client::{RawSftpSession, error::Error as SftpError};
use russh_sftp::protocol::{File, FileAttributes, OpenFlags, StatusCode};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

use super::opendal_store::{
    RemoteErrorContext, classify_opendal_error, profile_endpoint, profile_error, profile_password,
    run_remote,
};

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

pub fn sftp_store_from_profile<'a, R: CredentialResolver>(
    provider: ProviderId,
    profile: &'a ConnectionProfile,
    credentials: &'a R,
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

        if profile.credential().is_none() && host_key == HostKeyPolicy::KnownHosts {
            let mut builder = Sftp::default()
                .endpoint(&profile_endpoint(profile, "ssh", 22))
                .root(profile.path())
                .known_hosts_strategy("Strict");
            if let Some(username) = profile.username() {
                builder = builder.user(username);
            }
            let operator = Operator::new(builder).map_err(|error| {
                profile_error(
                    profile,
                    classify_opendal_error(&error, RemoteErrorContext::Connect),
                )
            })?;
            return OpendalStore::from_profile_operator(
                provider,
                profile,
                operator,
                RemoteCasePolicy::Unknown,
                RemoteMutationPolicy::CapabilitiesVerified,
            );
        }

        let secret = profile_password(profile, credentials, cancellation.clone()).await?;
        let username = profile
            .username()
            .ok_or_else(|| profile_error(profile, RemoteErrorCategory::InvalidProfile))?;
        let service = RusshSftpService::new(RusshSftpConfig {
            host: profile.host().as_str().to_owned(),
            port: profile.port().unwrap_or(22),
            username: username.to_owned(),
            root: profile.path().to_owned(),
            secret,
            host_key,
        });
        let warm_service = service.clone();
        run_remote(cancellation, async move { warm_service.warm_up().await })
            .await
            .map_err(|category| profile_error(profile, category))?
            .map_err(|category| profile_error(profile, category))?;
        let operator = Operator::new(RusshSftpBuilder::new(service)).map_err(|error| {
            profile_error(
                profile,
                classify_opendal_error(&error, RemoteErrorContext::Connect),
            )
        })?;
        OpendalStore::from_profile_operator(
            provider,
            profile,
            operator,
            RemoteCasePolicy::Unknown,
            RemoteMutationPolicy::CapabilitiesVerified,
        )
    })
}

struct RusshSftpConfig {
    host: String,
    port: u16,
    username: String,
    root: String,
    secret: Option<Zeroizing<String>>,
    host_key: HostKeyPolicy,
}

impl fmt::Debug for RusshSftpConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RusshSftpConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("root", &self.root)
            .field("has_secret", &self.secret.is_some())
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
    _ssh: client::Handle<SftpHostKeyVerifier>,
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
        ServiceInfo::new("sftp", &self.inner.config.root, &self.inner.config.host)
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
}

impl client::Handler for SftpHostKeyVerifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let public_key = server_public_key.public_key();
        Ok(match &self.policy {
            HostKeyPolicy::KnownHosts => {
                russh::keys::check_known_hosts(&self.host, self.port, &public_key).unwrap_or(false)
            }
            HostKeyPolicy::PinnedSha256(expected) => {
                Sha256::digest(public_key.public_key_bytes()).as_slice() == expected
            }
        })
    }
}

async fn connect_sftp(config: &RusshSftpConfig) -> OpendalResult<RusshConnection> {
    let verifier = SftpHostKeyVerifier {
        host: config.host.clone(),
        port: config.port,
        policy: config.host_key.clone(),
    };
    let mut ssh = tokio::time::timeout(
        Duration::from_secs(10),
        client::connect(
            Arc::new(client::Config::default()),
            (config.host.as_str(), config.port),
            verifier,
        ),
    )
    .await
    .map_err(|_| temporary_sftp_error())?
    .map_err(|error| map_ssh_error(&error))?;
    let authenticated = match config.secret.as_deref() {
        Some(secret) if secret.trim_start().starts_with("-----BEGIN") => {
            let key = decode_secret_key(secret, None).map_err(|_| {
                Error::new(ErrorKind::PermissionDenied, "SFTP private key is invalid")
            })?;
            ssh.authenticate_publickey(
                config.username.clone(),
                PrivateKeyWithHashAlg::new(Arc::new(key), None),
            )
            .await
        }
        Some(secret) => {
            ssh.authenticate_password(config.username.clone(), secret.to_owned())
                .await
        }
        None => ssh.authenticate_none(config.username.clone()).await,
    }
    .map_err(|_| Error::new(ErrorKind::PermissionDenied, "SFTP authentication failed"))?;
    if !authenticated.success() {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "SFTP authentication failed",
        ));
    }
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
        _ssh: ssh,
        sftp: raw,
    })
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
        russh::Error::UnknownKey | russh::Error::KeyChanged { .. } => Error::new(
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
