use futures_lite::future;
use musheen_core::{
    BoxFuture, CancellationToken, CapabilityKind, CapabilityMatrix, CapabilityReason,
    CapabilityState, Continuation, DirectoryWatch, DisplayPath, ItemId, ItemKind, MutationRequest,
    Page, PageRequest, ProviderId, Store, StoreError, StoreItem, StorePath, TotalHint,
};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const WORK_QUEUE_CAPACITY: usize = 64;
const WORKER_COUNT: usize = 2;
const SYNC_CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmbBackendErrorKind {
    Retryable,
    Authentication,
    Permission,
    NotFound,
    Conflict,
    Unsupported,
    Permanent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmbBackendError {
    kind: SmbBackendErrorKind,
}

impl SmbBackendError {
    #[must_use]
    pub const fn new(kind: SmbBackendErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> SmbBackendErrorKind {
        self.kind
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmbEntry {
    path: Box<str>,
    name: Box<str>,
    kind: ItemKind,
    size: Option<u64>,
    modified_unix_seconds: Option<i64>,
    stable_key: Option<Box<[u8]>>,
}

impl SmbEntry {
    #[must_use]
    pub fn new(
        path: impl Into<Box<str>>,
        name: impl Into<Box<str>>,
        kind: ItemKind,
        size: Option<u64>,
        modified_unix_seconds: Option<i64>,
    ) -> Self {
        Self {
            path: path.into(),
            name: name.into(),
            kind,
            size,
            modified_unix_seconds,
            stable_key: None,
        }
    }

    #[must_use]
    pub fn with_stable_key(mut self, stable_key: impl Into<Box<[u8]>>) -> Self {
        self.stable_key = Some(stable_key.into());
        self
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SmbShare(Box<str>);

impl SmbShare {
    #[must_use]
    pub fn new(name: impl Into<Box<str>>) -> Self {
        Self(name.into())
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.0
    }
}

/// Blocking SMB boundary. Implementations are called only on the dedicated
/// Musheen SMB worker through `SmbStore`.
pub trait SmbBackend: Send + Sync + 'static {
    fn list_directory(&self, path: &str) -> Result<Vec<SmbEntry>, SmbBackendError>;
    fn resolve(&self, path: &str) -> Result<Option<SmbEntry>, SmbBackendError>;
    fn rename(&self, source: &str, destination: &str) -> Result<(), SmbBackendError>;
    fn list_shares(&self) -> Result<Vec<SmbShare>, SmbBackendError>;
    fn reconnect(&self) -> Result<(), SmbBackendError>;
}

type Work = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone)]
struct SmbBlockingPool {
    sender: SyncSender<Work>,
}

impl SmbBlockingPool {
    fn shared() -> Result<Self, StoreError> {
        static POOL: OnceLock<SmbBlockingPool> = OnceLock::new();
        if let Some(pool) = POOL.get() {
            return Ok(pool.clone());
        }
        let pool = Self::new()?;
        if POOL.set(pool.clone()).is_err() {
            return Ok(POOL
                .get()
                .expect("the competing SMB pool was stored")
                .clone());
        }
        Ok(pool)
    }

    fn new() -> Result<Self, StoreError> {
        let (sender, receiver) = sync_channel::<Work>(WORK_QUEUE_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..WORKER_COUNT {
            let receiver = Arc::clone(&receiver);
            std::thread::Builder::new()
                .name(format!("musheen-smb-{index}"))
                .spawn(move || {
                    loop {
                        let work = receiver
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .recv();
                        let Ok(work) = work else {
                            break;
                        };
                        work();
                    }
                })
                .map_err(|_| StoreError::Backend("the SMB worker could not start".into()))?;
        }
        Ok(Self { sender })
    }

    fn submit<T: Send + 'static>(
        &self,
        operation: impl FnOnce() -> T + Send + 'static,
    ) -> Result<async_channel::Receiver<T>, StoreError> {
        let (sender, receiver) = async_channel::bounded(1);
        let work = Box::new(move || {
            let _ = sender.send_blocking(operation());
        });
        self.enqueue(work)?;
        Ok(receiver)
    }

    fn enqueue(&self, work: Work) -> Result<(), StoreError> {
        self.sender.try_send(work).map_err(|error| match error {
            TrySendError::Full(_) => StoreError::ResourceLimit {
                resource: "queued SMB operations",
                value: WORK_QUEUE_CAPACITY + 1,
                maximum: WORK_QUEUE_CAPACITY,
            },
            TrySendError::Disconnected(_) => {
                StoreError::Backend("the SMB worker is unavailable".into())
            }
        })
    }

    async fn execute<T: Send + 'static>(
        &self,
        cancellation: CancellationToken,
        operation: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, StoreError> {
        cancellation.check()?;
        let receiver = self.submit(operation)?;
        let completed = async move {
            receiver
                .recv()
                .await
                .map_err(|_| StoreError::Backend("the SMB worker stopped unexpectedly".into()))
        };
        let cancelled = async move {
            future::poll_fn(|context| {
                if cancellation.is_cancelled() {
                    std::task::Poll::Ready(())
                } else {
                    cancellation.register_waker(context.waker());
                    std::task::Poll::Pending
                }
            })
            .await;
            Err(StoreError::Cancelled)
        };
        future::race(completed, cancelled).await
    }

    async fn execute_mutation<T: Send + 'static>(
        &self,
        cancellation: CancellationToken,
        operation: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, StoreError> {
        cancellation.check()?;
        let worker_cancellation = cancellation.clone();
        self.submit(move || {
            worker_cancellation.check()?;
            Ok(operation())
        })?
        .recv()
        .await
        .map_err(|_| StoreError::Backend("the SMB worker stopped unexpectedly".into()))
        .and_then(std::convert::identity)
    }

    fn execute_sync<T: Send + 'static>(
        &self,
        operation: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, StoreError> {
        let (sender, receiver) = sync_channel(1);
        let work = Box::new(move || {
            let _ = sender.send(operation());
        });
        self.enqueue(work)?;
        receiver
            .recv_timeout(SYNC_CALL_TIMEOUT)
            .map_err(|_| StoreError::Backend("the synchronous SMB request timed out".into()))
    }
}

#[derive(Clone)]
pub struct SmbStore {
    provider: ProviderId,
    backend: Arc<dyn SmbBackend>,
    pool: SmbBlockingPool,
}

impl SmbStore {
    pub fn new(provider: ProviderId, backend: Arc<dyn SmbBackend>) -> Result<Self, StoreError> {
        Ok(Self {
            provider,
            backend,
            pool: SmbBlockingPool::shared()?,
        })
    }

    #[must_use]
    pub fn root_path(&self) -> StorePath {
        self.path("/").expect("the built-in SMB root path is valid")
    }

    pub fn path(&self, path: &str) -> Result<StorePath, StoreError> {
        let normalized = normalize_path(path)?;
        StorePath::from_provider_key(self.provider.clone(), normalized.into_bytes())
            .map_err(|_| StoreError::Backend("the SMB path exceeds the provider limit".into()))
    }

    pub fn list_shares(
        &self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<Vec<SmbShare>, StoreError>> {
        let backend = Arc::clone(&self.backend);
        let pool = self.pool.clone();
        Box::pin(async move {
            pool.execute(cancellation, move || backend.list_shares())
                .await?
                .map_err(store_backend_error)
        })
    }

    fn remote_path(&self, path: &StorePath) -> Result<String, StoreError> {
        let (provider, key) = path
            .provider_key()
            .ok_or_else(|| StoreError::Backend("SMB requires a provider path".into()))?;
        if provider != &self.provider {
            return Err(StoreError::Backend(
                "SMB path belongs to another provider".into(),
            ));
        }
        let path = std::str::from_utf8(key)
            .map_err(|_| StoreError::Backend("SMB paths must be valid UTF-8".into()))?;
        normalize_path(path)
    }

    fn item(&self, entry: SmbEntry) -> Result<StoreItem, StoreError> {
        let path = self.path(&entry.path)?;
        let key = entry
            .stable_key
            .unwrap_or_else(|| entry.path.as_bytes().to_vec().into_boxed_slice());
        let id = ItemId::new(self.provider.clone(), key).map_err(|_| {
            StoreError::Backend("SMB item identity exceeds the provider limit".into())
        })?;
        let mut item = StoreItem::new(
            id,
            path,
            DisplayPath::new(entry.name),
            entry.kind,
            entry.size,
        );
        if let Some(modified) = entry.modified_unix_seconds {
            item = item.with_modified_unix_seconds(modified);
        }
        Ok(item)
    }
}

impl Store for SmbStore {
    fn provider_id(&self) -> &ProviderId {
        &self.provider
    }

    fn capabilities(&self, _location: &StorePath) -> CapabilityMatrix {
        CapabilityMatrix::new(|kind| match kind {
            CapabilityKind::AtomicRename => CapabilityState::Supported,
            CapabilityKind::CaseSensitivity => unsupported("SMB names are case-insensitive"),
            CapabilityKind::Watching => unsupported("SMB changes require manual refresh"),
            CapabilityKind::Permissions | CapabilityKind::Ownership => {
                unknown("the SMB adapter does not expose complete Windows ACL metadata")
            }
            _ => unknown("the SMB server capability has not been safely established"),
        })
    }

    fn resolve_item(&self, path: &StorePath) -> Result<Option<StoreItem>, StoreError> {
        let remote_path = self.remote_path(path)?;
        let backend = Arc::clone(&self.backend);
        let entry = self
            .pool
            .execute_sync(move || {
                read_with_reconnect(&*backend, |backend| backend.resolve(&remote_path))
            })?
            .map_err(store_backend_error)?;
        entry.map(|entry| self.item(entry)).transpose()
    }

    fn location_writable(&self, _path: &StorePath) -> Result<CapabilityState, StoreError> {
        Ok(unknown(
            "the SMB adapter cannot prove directory ACL access before a mutation",
        ))
    }

    fn read_directory<'a>(
        &'a self,
        location: &'a StorePath,
        request: PageRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Page<StoreItem>, StoreError>> {
        let remote_path = self.remote_path(location);
        let backend = Arc::clone(&self.backend);
        let pool = self.pool.clone();
        let store = self.clone();
        Box::pin(async move {
            let remote_path = remote_path?;
            let entries = pool
                .execute(cancellation.clone(), move || {
                    read_with_reconnect(&*backend, |backend| backend.list_directory(&remote_path))
                })
                .await?
                .map_err(store_backend_error)?;
            cancellation.check()?;
            let offset = request
                .continuation()
                .map(Continuation::decode_usize)
                .transpose()?
                .unwrap_or(0);
            if offset > entries.len() {
                return Err(StoreError::InvalidContinuation);
            }
            let end = offset
                .saturating_add(request.page_size())
                .min(entries.len());
            let items = entries[offset..end]
                .iter()
                .cloned()
                .map(|entry| store.item(entry))
                .collect::<Result<Vec<_>, _>>()?;
            let next = (end < entries.len()).then(|| Continuation::from_usize(end));
            Page::try_new(
                &request,
                items,
                next,
                TotalHint::Exact(entries.len() as u64),
            )
        })
    }

    fn watch_directory<'a>(
        &'a self,
        _location: &'a StorePath,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn DirectoryWatch>, StoreError>> {
        Box::pin(async move {
            cancellation.check()?;
            Err(StoreError::unsupported(
                "watch_directory",
                "SMB changes require manual refresh",
            ))
        })
    }

    fn validate_mutation(&self, request: &MutationRequest) -> Result<(), StoreError> {
        match request {
            MutationRequest::Rename { .. } | MutationRequest::Move { .. } => Ok(()),
            _ => {
                Err(request.unsupported("the SMB adapter currently proves only server-side rename"))
            }
        }
    }

    fn mutate<'a>(
        &'a self,
        request: MutationRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        let validation = self.validate_mutation(&request);
        let paths = match &request {
            MutationRequest::Rename {
                source,
                destination,
            }
            | MutationRequest::Move {
                source,
                destination,
            } => self.remote_path(source).and_then(|source| {
                self.remote_path(destination)
                    .map(|destination| (source, destination))
            }),
            _ => Err(StoreError::Backend("unsupported SMB mutation".into())),
        };
        let backend = Arc::clone(&self.backend);
        let pool = self.pool.clone();
        Box::pin(async move {
            validation?;
            let (source, destination) = paths?;
            pool.execute_mutation(cancellation, move || backend.rename(&source, &destination))
                .await?
                .map_err(store_backend_error)
        })
    }
}

fn read_with_reconnect<T>(
    backend: &dyn SmbBackend,
    operation: impl Fn(&dyn SmbBackend) -> Result<T, SmbBackendError>,
) -> Result<T, SmbBackendError> {
    match operation(backend) {
        Err(error) if error.kind() == SmbBackendErrorKind::Retryable => {
            backend.reconnect()?;
            operation(backend)
        }
        result => result,
    }
}

fn normalize_path(path: &str) -> Result<String, StoreError> {
    let mut normalized = path.replace('\\', "/");
    if !normalized.starts_with('/') {
        normalized.insert(0, '/');
    }
    if normalized.contains('\0') || normalized.split('/').any(|segment| segment == "..") {
        return Err(StoreError::Backend("SMB path is invalid".into()));
    }
    Ok(normalized)
}

fn store_backend_error(error: SmbBackendError) -> StoreError {
    let message = match error.kind() {
        SmbBackendErrorKind::Retryable => "SMB service is temporarily unavailable",
        SmbBackendErrorKind::Authentication => "SMB authentication failed",
        SmbBackendErrorKind::Permission => "SMB permission was denied",
        SmbBackendErrorKind::NotFound => "SMB item was not found",
        SmbBackendErrorKind::Conflict => "SMB destination conflicts with an existing item",
        SmbBackendErrorKind::Unsupported => "SMB operation is unsupported",
        SmbBackendErrorKind::Permanent => "SMB operation failed",
    };
    StoreError::Backend(message.into())
}

fn unsupported(reason: &'static str) -> CapabilityState {
    CapabilityState::Unsupported(
        CapabilityReason::new(reason).expect("the built-in SMB capability reason is valid"),
    )
}

fn unknown(reason: &'static str) -> CapabilityState {
    CapabilityState::Unknown(
        CapabilityReason::new(reason).expect("the built-in SMB capability reason is valid"),
    )
}

#[cfg(feature = "smb-pavao")]
mod pavao_backend {
    use super::*;
    use pavao::{
        SmbClient, SmbCredentials, SmbDialect, SmbDirentType, SmbEncryptionLevel, SmbOptions,
    };
    use std::sync::Mutex;
    use std::time::UNIX_EPOCH;
    use zeroize::Zeroizing;

    pub struct PavaoConfig {
        server: Box<str>,
        share: Box<str>,
        username: Box<str>,
        password: Zeroizing<String>,
        workgroup: Box<str>,
        require_encryption: bool,
    }

    impl PavaoConfig {
        #[must_use]
        pub fn new(
            server: impl Into<Box<str>>,
            share: impl Into<Box<str>>,
            username: impl Into<Box<str>>,
            password: Zeroizing<String>,
            workgroup: impl Into<Box<str>>,
            require_encryption: bool,
        ) -> Self {
            Self {
                server: server.into(),
                share: share.into(),
                username: username.into(),
                password,
                workgroup: workgroup.into(),
                require_encryption,
            }
        }
    }

    struct PavaoBackend {
        config: PavaoConfig,
        client: Mutex<Option<SmbClient>>,
    }

    impl PavaoBackend {
        fn client<T>(
            &self,
            operation: impl FnOnce(&SmbClient) -> Result<T, pavao::SmbError>,
        ) -> Result<T, SmbBackendError> {
            let mut client = self
                .client
                .lock()
                .map_err(|_| SmbBackendError::new(SmbBackendErrorKind::Permanent))?;
            if client.is_none() {
                *client = Some(build_client(&self.config)?);
            }
            operation(client.as_ref().expect("the Pavão client was initialized"))
                .map_err(classify_pavao_error)
        }
    }

    impl SmbBackend for PavaoBackend {
        fn list_directory(&self, path: &str) -> Result<Vec<SmbEntry>, SmbBackendError> {
            self.client(|client| client.list_dirplus(path))
                .map(|entries| {
                    entries
                        .into_iter()
                        .map(|entry| {
                            let child = join_path(path, entry.name());
                            let kind = match entry.get_type() {
                                SmbDirentType::Dir => ItemKind::Directory,
                                SmbDirentType::File => ItemKind::RegularFile,
                                SmbDirentType::Link => ItemKind::SymbolicLink,
                                _ => ItemKind::Other,
                            };
                            let modified = entry
                                .mtime
                                .duration_since(UNIX_EPOCH)
                                .ok()
                                .and_then(|duration| i64::try_from(duration.as_secs()).ok());
                            SmbEntry::new(child, entry.name, kind, Some(entry.size), modified)
                        })
                        .collect()
                })
        }

        fn resolve(&self, path: &str) -> Result<Option<SmbEntry>, SmbBackendError> {
            match self.client(|client| client.stat(path)) {
                Ok(stat) => {
                    let name = path
                        .rsplit('/')
                        .find(|segment| !segment.is_empty())
                        .unwrap_or("/");
                    let kind = if stat.mode.is_dir() {
                        ItemKind::Directory
                    } else if stat.mode.is_file() {
                        ItemKind::RegularFile
                    } else {
                        ItemKind::Other
                    };
                    let modified = stat
                        .modified
                        .duration_since(UNIX_EPOCH)
                        .ok()
                        .and_then(|duration| i64::try_from(duration.as_secs()).ok());
                    Ok(Some(SmbEntry::new(
                        path,
                        name,
                        kind,
                        Some(stat.size),
                        modified,
                    )))
                }
                Err(error) if error.kind() == SmbBackendErrorKind::NotFound => Ok(None),
                Err(error) => Err(error),
            }
        }

        fn rename(&self, source: &str, destination: &str) -> Result<(), SmbBackendError> {
            self.client(|client| client.rename(source, destination))
        }

        fn list_shares(&self) -> Result<Vec<SmbShare>, SmbBackendError> {
            let credentials = SmbCredentials::default()
                .server(&self.config.server)
                .username(&self.config.username)
                .password(self.config.password.as_str())
                .workgroup(&self.config.workgroup);
            let client = SmbClient::new(credentials, options(&self.config))
                .map_err(classify_pavao_connect_error)?;
            client
                .list_dir("/")
                .map_err(classify_pavao_error)
                .map(|entries| {
                    entries
                        .into_iter()
                        .filter(|entry| entry.get_type() == SmbDirentType::FileShare)
                        .map(|entry| SmbShare::new(entry.name()))
                        .collect()
                })
        }

        fn reconnect(&self) -> Result<(), SmbBackendError> {
            *self
                .client
                .lock()
                .map_err(|_| SmbBackendError::new(SmbBackendErrorKind::Permanent))? = None;
            Ok(())
        }
    }

    impl SmbStore {
        pub fn from_pavao(provider: ProviderId, config: PavaoConfig) -> Result<Self, StoreError> {
            Self::new(
                provider,
                Arc::new(PavaoBackend {
                    config,
                    client: Mutex::new(None),
                }),
            )
        }
    }

    fn build_client(config: &PavaoConfig) -> Result<SmbClient, SmbBackendError> {
        let credentials = SmbCredentials::default()
            .server(&config.server)
            .share(&config.share)
            .username(&config.username)
            .password(config.password.as_str())
            .workgroup(&config.workgroup);
        SmbClient::new(credentials, options(config)).map_err(classify_pavao_connect_error)
    }

    fn options(config: &PavaoConfig) -> SmbOptions {
        let encryption = if config.require_encryption {
            SmbEncryptionLevel::Require
        } else {
            SmbEncryptionLevel::Request
        };
        SmbOptions::default()
            .case_sensitive(false)
            .min_protocol(SmbDialect::Smb202)
            .max_protocol(SmbDialect::Smb311)
            .encryption_level(encryption)
    }

    fn classify_pavao_error(error: pavao::SmbError) -> SmbBackendError {
        let kind = match error {
            pavao::SmbError::Io(error) => match error.kind() {
                std::io::ErrorKind::NotFound => SmbBackendErrorKind::NotFound,
                std::io::ErrorKind::PermissionDenied => SmbBackendErrorKind::Permission,
                std::io::ErrorKind::AlreadyExists => SmbBackendErrorKind::Conflict,
                std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::NotConnected
                | std::io::ErrorKind::WouldBlock => SmbBackendErrorKind::Retryable,
                std::io::ErrorKind::Unsupported => SmbBackendErrorKind::Unsupported,
                _ => SmbBackendErrorKind::Permanent,
            },
            pavao::SmbError::ProtocolConfiguration
            | pavao::SmbError::ProtocolConfigurationConflict
            | pavao::SmbError::InvalidProtocolRange { .. } => SmbBackendErrorKind::Unsupported,
            _ => SmbBackendErrorKind::Permanent,
        };
        SmbBackendError::new(kind)
    }

    fn classify_pavao_connect_error(error: pavao::SmbError) -> SmbBackendError {
        if matches!(
            error,
            pavao::SmbError::Io(ref error)
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ) {
            SmbBackendError::new(SmbBackendErrorKind::Authentication)
        } else {
            classify_pavao_error(error)
        }
    }

    fn join_path(parent: &str, name: &str) -> String {
        format!("{}/{}", parent.trim_end_matches('/'), name)
    }
}

#[cfg(feature = "smb-pavao")]
pub use pavao_backend::PavaoConfig;
