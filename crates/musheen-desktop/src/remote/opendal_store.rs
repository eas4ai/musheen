use super::{
    ConnectionProfile, CredentialResolver, ProviderPool, RemoteConnector, RemoteError,
    RemoteErrorCategory, RemoteProtocol,
};
use futures_lite::{StreamExt, future};
use futures_rustls::rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use futures_rustls::rustls::crypto::{
    WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
};
use futures_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use futures_rustls::rustls::{
    ClientConfig, DigitallySignedStruct, Error as TlsError, SignatureScheme,
};
use musheen_core::{
    BoxFuture, CancellationToken, CapabilityKind, CapabilityMatrix, CapabilityReason,
    CapabilityState, Continuation, DirectoryWatch, DisplayPath, ItemId, ItemKind, MutationRequest,
    Page, PageRequest, ProviderId, Store, StoreError, StoreItem, StorePath, TotalHint,
};
use musheen_ops::StagingPath;
use opendal::{Entry, Error, ErrorKind, Lister, Metadata, Operator};
use opendal::{HttpTransporter, OperationContext};
use opendal_http_transport_reqwest::ReqwestTransport;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::future::Future;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroize::Zeroizing;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const TRANSFER_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const TRANSFER_CHUNK_BYTES: usize = 1024 * 1024;
const MAX_LIST_SESSIONS: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteCasePolicy {
    Sensitive,
    Insensitive,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteMutationPolicy {
    ReadOnly,
    CapabilitiesVerified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteErrorContext {
    Connect,
    Authentication,
    Read,
    Mutation,
}

#[must_use]
pub fn classify_opendal_error(error: &Error, context: RemoteErrorContext) -> RemoteErrorCategory {
    if context == RemoteErrorContext::Authentication && error.kind() == ErrorKind::PermissionDenied
    {
        return RemoteErrorCategory::Authentication;
    }
    if error.is_temporary() || error.kind() == ErrorKind::RateLimited {
        return RemoteErrorCategory::Retryable;
    }
    if matches!(
        error.kind(),
        ErrorKind::AlreadyExists | ErrorKind::ConditionNotMatch | ErrorKind::IsSameFile
    ) {
        return RemoteErrorCategory::Conflict;
    }
    // OpenDAL has no quota-specific kind. Inspect only ephemerally; the text is
    // never retained by RemoteError or StoreError.
    let diagnostic = error.to_string().to_ascii_lowercase();
    if diagnostic.contains("quota")
        || diagnostic.contains("insufficient storage")
        || diagnostic.contains("status: 507")
        || diagnostic.contains("status code: 507")
    {
        return RemoteErrorCategory::Quota;
    }
    match error.kind() {
        ErrorKind::PermissionDenied => RemoteErrorCategory::Permission,
        ErrorKind::Unsupported => RemoteErrorCategory::Unsupported,
        _ => RemoteErrorCategory::Permanent,
    }
}

pub(crate) fn profile_error(
    profile: &ConnectionProfile,
    category: RemoteErrorCategory,
) -> RemoteError {
    RemoteError::new(profile.protocol(), category, Some(profile.host().clone()))
}

pub(crate) fn profile_endpoint(
    profile: &ConnectionProfile,
    scheme: &str,
    default_port: u16,
) -> String {
    let host = if profile.host().as_str().contains(':') {
        format!("[{}]", profile.host().as_str())
    } else {
        profile.host().as_str().to_owned()
    };
    match profile.port() {
        Some(port) if port != default_port => format!("{scheme}://{host}:{port}"),
        _ => format!("{scheme}://{host}"),
    }
}

pub(crate) async fn profile_password<R: CredentialResolver>(
    profile: &ConnectionProfile,
    credentials: &R,
    cancellation: CancellationToken,
) -> Result<Option<Zeroizing<String>>, RemoteError> {
    let Some(reference) = profile.credential() else {
        return Ok(None);
    };
    let secret = credentials
        .resolve(reference, cancellation)
        .await
        .map_err(|category| profile_error(profile, category))?;
    secret.expose_secret(|bytes| {
        std::str::from_utf8(bytes)
            .map(|value| Some(Zeroizing::new(value.to_owned())))
            .map_err(|_| profile_error(profile, RemoteErrorCategory::Authentication))
    })
}

pub(crate) fn with_pinned_http_transport(
    operator: Operator,
    profile: &ConnectionProfile,
    pin: [u8; 32],
) -> Result<Operator, RemoteError> {
    let provider = futures_rustls::rustls::crypto::ring::default_provider();
    let algorithms = provider.signature_verification_algorithms;
    let config = ClientConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|_| profile_error(profile, RemoteErrorCategory::Tls))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedHttpVerifier { pin, algorithms }))
        .with_no_client_auth();
    let client = reqwest::Client::builder()
        .tls_backend_preconfigured(config)
        .build()
        .map_err(|_| profile_error(profile, RemoteErrorCategory::Tls))?;
    let transport = HttpTransporter::new(ReqwestTransport::new(client));
    let context = OperationContext::new().with_http_transport(transport);
    Ok(operator.with_context(context))
}

#[derive(Debug)]
struct PinnedHttpVerifier {
    pin: [u8; 32],
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedHttpVerifier {
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

struct ListSession {
    location: String,
    lister: Lister,
    buffered: Option<Entry>,
    _lease: Option<super::PoolLease<Operator>>,
}

#[derive(Clone)]
struct OperatorConnector {
    operator: Operator,
}

impl RemoteConnector for OperatorConnector {
    type Connection = Operator;

    fn connect<'a>(
        &'a self,
        _profile: &'a ConnectionProfile,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Self::Connection, RemoteErrorCategory>> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                Err(RemoteErrorCategory::Cancelled)
            } else {
                Ok(self.operator.clone())
            }
        })
    }
}

#[derive(Clone)]
pub struct OpendalStore {
    provider: ProviderId,
    protocol: RemoteProtocol,
    operator: Operator,
    pool: Option<ProviderPool<OperatorConnector>>,
    capabilities: CapabilityMatrix,
    mutation_policy: RemoteMutationPolicy,
    root: StorePath,
    sessions: Arc<tokio::sync::Mutex<HashMap<u64, ListSession>>>,
    next_session: Arc<AtomicU64>,
}

impl OpendalStore {
    pub fn from_operator(
        provider: ProviderId,
        protocol: RemoteProtocol,
        operator: Operator,
        case_policy: RemoteCasePolicy,
        mutation_policy: RemoteMutationPolicy,
    ) -> Result<Self, RemoteError> {
        let root = StorePath::from_provider_key(provider.clone(), b"/".to_vec())
            .map_err(|_| RemoteError::new(protocol, RemoteErrorCategory::InvalidProfile, None))?;
        let native = operator.info().capability();
        let unsupported = || {
            CapabilityState::Unsupported(
                CapabilityReason::new("the remote service does not expose this capability")
                    .expect("the static capability reason is valid"),
            )
        };
        let unknown = || {
            CapabilityState::Unknown(
                CapabilityReason::new("the remote service does not prove this capability")
                    .expect("the static capability reason is valid"),
            )
        };
        let capabilities = CapabilityMatrix::new(|kind| match kind {
            CapabilityKind::CaseSensitivity => match case_policy {
                RemoteCasePolicy::Sensitive => CapabilityState::Supported,
                RemoteCasePolicy::Insensitive => unsupported(),
                RemoteCasePolicy::Unknown => unknown(),
            },
            CapabilityKind::AtomicRename if native.rename => unknown(),
            CapabilityKind::Watching => unsupported(),
            _ => unsupported(),
        });
        Ok(Self {
            provider,
            protocol,
            operator,
            pool: None,
            capabilities,
            mutation_policy,
            root,
            sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            next_session: Arc::new(AtomicU64::new(1)),
        })
    }

    pub(crate) fn from_profile_operator(
        provider: ProviderId,
        profile: &ConnectionProfile,
        operator: Operator,
        case_policy: RemoteCasePolicy,
        mutation_policy: RemoteMutationPolicy,
    ) -> Result<Self, RemoteError> {
        let pool = ProviderPool::new(
            profile.clone(),
            OperatorConnector {
                operator: operator.clone(),
            },
        )?;
        let mut store = Self::from_operator(
            provider,
            profile.protocol(),
            operator,
            case_policy,
            mutation_policy,
        )?;
        store.pool = Some(pool);
        Ok(store)
    }

    #[must_use]
    pub fn root_path(&self) -> StorePath {
        self.root.clone()
    }

    pub fn path(&self, path: &str) -> Result<StorePath, StoreError> {
        let normalized = normalize_store_path(path)?;
        StorePath::from_provider_key(self.provider.clone(), normalized.into_bytes())
            .map_err(|_| StoreError::Backend("remote path exceeds provider limits".into()))
    }

    #[must_use]
    pub fn supports_exclusive_publish(&self) -> bool {
        self.mutation_policy == RemoteMutationPolicy::CapabilitiesVerified
            && self.operator.info().capability().write_with_if_not_exists
    }

    pub fn read_range<'a>(
        &'a self,
        path: &'a StorePath,
        range: Range<u64>,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<u8>, RemoteError>> {
        let remote_path = match self.remote_path(path) {
            Ok(path) => path,
            Err(category) => {
                return Box::pin(
                    async move { Err(RemoteError::new(self.protocol, category, None)) },
                );
            }
        };
        let operator = self.operator.clone();
        let pool = self.pool.clone();
        let protocol = self.protocol;
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(RemoteError::new(
                    protocol,
                    RemoteErrorCategory::Cancelled,
                    None,
                ));
            }
            let lease = match pool {
                Some(pool) => Some(
                    pool.acquire(cancellation.clone())
                        .await
                        .map_err(|error| RemoteError::new(protocol, error.category(), None))?,
                ),
                None => None,
            };
            let operator = lease
                .as_ref()
                .map_or(operator, |lease| lease.connection().clone());
            let result = run_remote(cancellation.clone(), async move {
                operator.read_with(&remote_path).range(range).await
            })
            .await
            .map_err(|category| RemoteError::new(protocol, category, None))?;
            if cancellation.is_cancelled() {
                return Err(RemoteError::new(
                    protocol,
                    RemoteErrorCategory::Cancelled,
                    None,
                ));
            }
            result.map(|buffer| buffer.to_vec()).map_err(|error| {
                RemoteError::new(
                    protocol,
                    classify_opendal_error(&error, RemoteErrorContext::Read),
                    None,
                )
            })
        })
    }

    /// Uploads to a job-owned sibling staging key. It never writes the final
    /// destination and refuses an existing staging object.
    pub fn upload_staging_from_local<'a>(
        &'a self,
        local: &'a Path,
        staging: &'a StagingPath,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<u64, RemoteError>> {
        let remote_path = self.remote_path(staging.path());
        let owned = staging.is_app_owned();
        let writable = self.mutation_policy == RemoteMutationPolicy::CapabilitiesVerified
            && self.operator.info().capability().write;
        let operator = self.operator.clone();
        let pool = self.pool.clone();
        let protocol = self.protocol;
        let local = local.to_path_buf();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(RemoteError::new(
                    protocol,
                    RemoteErrorCategory::Cancelled,
                    None,
                ));
            }
            if !writable {
                return Err(RemoteError::new(
                    protocol,
                    RemoteErrorCategory::Unsupported,
                    None,
                ));
            }
            if !owned {
                return Err(RemoteError::new(
                    protocol,
                    RemoteErrorCategory::InvalidProfile,
                    None,
                ));
            }
            let remote_path =
                remote_path.map_err(|category| RemoteError::new(protocol, category, None))?;
            run_pooled_stream(
                operator,
                pool,
                protocol,
                cancellation,
                move |operator| async move { upload_staging(operator, &remote_path, &local).await },
            )
            .await
        })
    }

    /// Publishes an owned staging object only when the backend supports an
    /// exclusive destination write. Staging remains available for recovery.
    pub fn publish_staging_noreplace<'a>(
        &'a self,
        staging: &'a StagingPath,
        destination: &'a StorePath,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<u64, RemoteError>> {
        let staging_path = self.remote_path(staging.path());
        let destination_path = self.remote_path(destination);
        let writable = self.supports_exclusive_publish();
        let owned_sibling = staging.is_sibling_of(destination);
        let operator = self.operator.clone();
        let pool = self.pool.clone();
        let protocol = self.protocol;
        Box::pin(async move {
            if !writable {
                return Err(RemoteError::new(
                    protocol,
                    RemoteErrorCategory::Unsupported,
                    None,
                ));
            }
            if !owned_sibling {
                return Err(RemoteError::new(
                    protocol,
                    RemoteErrorCategory::InvalidProfile,
                    None,
                ));
            }
            let staging_path =
                staging_path.map_err(|category| RemoteError::new(protocol, category, None))?;
            let destination_path =
                destination_path.map_err(|category| RemoteError::new(protocol, category, None))?;
            run_pooled_stream(
                operator,
                pool,
                protocol,
                cancellation,
                move |operator| async move {
                    publish_staging_new(operator, &staging_path, &destination_path).await
                },
            )
            .await
        })
    }

    /// Downloads a regular file to a new local staging path. The caller owns
    /// cleanup of any partial file after an error or cancellation.
    pub fn download_to_new_local<'a>(
        &'a self,
        source: &'a StorePath,
        local: &'a Path,
        max_bytes: u64,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<u64, RemoteError>> {
        let remote_path = self.remote_path(source);
        let operator = self.operator.clone();
        let pool = self.pool.clone();
        let protocol = self.protocol;
        let local = local.to_path_buf();
        Box::pin(async move {
            let remote_path =
                remote_path.map_err(|category| RemoteError::new(protocol, category, None))?;
            run_pooled_stream(
                operator,
                pool,
                protocol,
                cancellation,
                move |operator| async move {
                    download_new_local(operator, &remote_path, &local, max_bytes).await
                },
            )
            .await
        })
    }

    fn remote_path(&self, path: &StorePath) -> Result<String, RemoteErrorCategory> {
        let Some((provider, key)) = path.provider_key() else {
            return Err(RemoteErrorCategory::Permanent);
        };
        if provider != &self.provider {
            return Err(RemoteErrorCategory::Permanent);
        }
        let path = std::str::from_utf8(key).map_err(|_| RemoteErrorCategory::Permanent)?;
        Ok(to_opendal_path(path))
    }

    fn item(&self, path: String, metadata: &Metadata) -> Result<StoreItem, StoreError> {
        let store_path = self.path(&format!("/{path}"))?;
        let mut identity = blake3::Hasher::new();
        identity.update(b"musheen-opendal-item-v1\0");
        identity.update(self.provider.as_str().as_bytes());
        identity.update(b"\0");
        identity.update(path.as_bytes());
        identity.update(b"\0");
        if let Some(version) = metadata.version() {
            identity.update(version.as_bytes());
        }
        identity.update(b"\0");
        if let Some(etag) = metadata.etag() {
            identity.update(etag.as_bytes());
        }
        identity.update(&metadata.content_length().to_be_bytes());
        if let Some(modified) = metadata.last_modified() {
            identity.update(&modified.into_inner().as_nanosecond().to_be_bytes());
        }
        let id = ItemId::new(
            self.provider.clone(),
            identity.finalize().as_bytes().to_vec(),
        )
        .map_err(|_| StoreError::Backend("remote item identity is invalid".into()))?;
        let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("/");
        let kind = if metadata.is_dir() {
            ItemKind::Directory
        } else if metadata.is_file() {
            ItemKind::RegularFile
        } else {
            ItemKind::Other
        };
        let size = metadata.is_file().then(|| metadata.content_length());
        let mut item = StoreItem::new(id, store_path, DisplayPath::new(name), kind, size);
        if let Some(modified) = metadata.last_modified() {
            item = item.with_modified_unix_seconds(modified.into_inner().as_second());
        }
        Ok(item)
    }

    fn store_error(error: &Error, context: RemoteErrorContext) -> StoreError {
        match classify_opendal_error(error, context) {
            RemoteErrorCategory::Unsupported => StoreError::unsupported(
                "remote operation",
                "the remote service does not support this operation",
            ),
            _ => StoreError::Backend("remote provider operation failed".into()),
        }
    }

    fn mutation_supported(&self, request: &MutationRequest) -> bool {
        if self.mutation_policy == RemoteMutationPolicy::ReadOnly {
            return false;
        }
        let capability = self.operator.info().capability();
        match request {
            MutationRequest::CreateDirectory { .. } => capability.create_dir,
            MutationRequest::CreateFile { .. } => capability.write,
            MutationRequest::Rename { .. } | MutationRequest::Move { .. } => capability.rename,
            MutationRequest::Copy { .. } => capability.copy,
            MutationRequest::PermanentDelete { .. } => capability.delete,
            _ => false,
        }
    }
}

impl Store for OpendalStore {
    fn provider_id(&self) -> &ProviderId {
        &self.provider
    }

    fn capabilities(&self, _location: &StorePath) -> CapabilityMatrix {
        self.capabilities.clone()
    }

    fn resolve_item(&self, path: &StorePath) -> Result<Option<StoreItem>, StoreError> {
        let remote_path = self
            .remote_path(path)
            .map_err(|_| StoreError::Backend("path belongs to a different provider".into()))?;
        let operator = self.operator.clone();
        let stat_path = remote_path.clone();
        let result = block_on_remote(async move { operator.stat(&stat_path).await })?;
        match result {
            Ok(metadata) => Ok(Some(self.item(remote_path, &metadata)?)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Self::store_error(&error, RemoteErrorContext::Read)),
        }
    }

    fn location_writable(&self, _path: &StorePath) -> Result<CapabilityState, StoreError> {
        if self.mutation_policy == RemoteMutationPolicy::ReadOnly
            || !self.operator.info().capability().write
        {
            Ok(CapabilityState::Unsupported(
                CapabilityReason::new("the remote service does not prove write support")
                    .expect("the static capability reason is valid"),
            ))
        } else {
            Ok(CapabilityState::Supported)
        }
    }

    fn read_directory<'a>(
        &'a self,
        location: &'a StorePath,
        request: PageRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Page<StoreItem>, StoreError>> {
        let remote_path = self.remote_path(location);
        let operator = self.operator.clone();
        let pool = self.pool.clone();
        let sessions = Arc::clone(&self.sessions);
        let next_session = Arc::clone(&self.next_session);
        let store = self.clone();
        Box::pin(async move {
            cancellation.check()?;
            let remote_path = remote_path
                .map_err(|_| StoreError::Backend("path belongs to a different provider".into()))?;
            let continuation = request.continuation().map(decode_session).transpose()?;
            let lease = if continuation.is_none() {
                match pool {
                    Some(pool) => {
                        Some(pool.acquire(cancellation.clone()).await.map_err(|_| {
                            StoreError::Backend("remote provider pool failed".into())
                        })?)
                    }
                    None => None,
                }
            } else {
                None
            };
            let operator = lease
                .as_ref()
                .map_or(operator, |lease| lease.connection().clone());
            let page_size = request.page_size();
            let page = run_remote(cancellation.clone(), async move {
                let (session_id, mut session) = if let Some(session_id) = continuation {
                    let mut registry = sessions.lock().await;
                    let session = registry
                        .remove(&session_id)
                        .ok_or(StoreError::InvalidContinuation)?;
                    if session.location != remote_path {
                        registry.insert(session_id, session);
                        return Err(StoreError::InvalidContinuation);
                    }
                    (session_id, session)
                } else {
                    let session_id = next_session.fetch_add(1, Ordering::Relaxed);
                    let lister = operator
                        .lister(&directory_path(&remote_path))
                        .await
                        .map_err(|error| {
                            OpendalStore::store_error(&error, RemoteErrorContext::Read)
                        })?;
                    (
                        session_id,
                        ListSession {
                            location: remote_path.clone(),
                            lister,
                            buffered: None,
                            _lease: lease,
                        },
                    )
                };
                let mut entries = Vec::with_capacity(page_size);
                if let Some(entry) = session.buffered.take() {
                    entries.push(entry);
                }
                while entries.len() < page_size {
                    match session.lister.next().await {
                        Some(Ok(entry)) => entries.push(entry),
                        Some(Err(error)) => {
                            return Err(OpendalStore::store_error(
                                &error,
                                RemoteErrorContext::Read,
                            ));
                        }
                        None => break,
                    }
                }
                let has_more = match session.lister.next().await {
                    Some(Ok(entry)) => {
                        session.buffered = Some(entry);
                        true
                    }
                    Some(Err(error)) => {
                        return Err(OpendalStore::store_error(&error, RemoteErrorContext::Read));
                    }
                    None => false,
                };
                if has_more {
                    let mut registry = sessions.lock().await;
                    if registry.len() >= MAX_LIST_SESSIONS
                        && let Some(oldest) = registry.keys().min().copied()
                    {
                        registry.remove(&oldest);
                    }
                    registry.insert(session_id, session);
                }
                Ok::<_, StoreError>((entries, has_more.then(|| encode_session(session_id))))
            })
            .await
            .map_err(|category| match category {
                RemoteErrorCategory::Cancelled => StoreError::Cancelled,
                RemoteErrorCategory::Timeout => {
                    StoreError::Backend("remote provider request timed out".into())
                }
                _ => StoreError::Backend("remote provider runtime failed".into()),
            })??;
            cancellation.check()?;
            let items = page
                .0
                .into_iter()
                .map(|entry| store.item(entry.path().to_owned(), entry.metadata()))
                .collect::<Result<Vec<_>, _>>()?;
            Page::try_new(&request, items, page.1, TotalHint::Unknown)
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
                "OpenDAL remote services require manual refresh",
            ))
        })
    }

    fn validate_mutation(&self, request: &MutationRequest) -> Result<(), StoreError> {
        if self.mutation_supported(request) {
            Ok(())
        } else {
            Err(request.unsupported("the remote service did not prove mutation support"))
        }
    }

    fn mutate<'a>(
        &'a self,
        request: MutationRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        let validation = self.validate_mutation(&request);
        let operator = self.operator.clone();
        let pool = self.pool.clone();
        let paths = mutation_paths(self, &request);
        Box::pin(async move {
            validation?;
            cancellation.check()?;
            let (source, destination) = paths?;
            let lease = match pool {
                Some(pool) => Some(
                    pool.acquire(cancellation.clone())
                        .await
                        .map_err(|_| StoreError::Backend("remote provider pool failed".into()))?,
                ),
                None => None,
            };
            let operator = lease
                .as_ref()
                .map_or(operator, |lease| lease.connection().clone());
            let result = run_remote(cancellation.clone(), async move {
                match request {
                    MutationRequest::CreateDirectory { .. } => {
                        operator.create_dir(&directory_path(&destination)).await
                    }
                    MutationRequest::CreateFile { .. } => operator
                        .write(&destination, Vec::<u8>::new())
                        .await
                        .map(|_| ()),
                    MutationRequest::Rename { .. } | MutationRequest::Move { .. } => {
                        operator
                            .rename(source.as_deref().unwrap_or_default(), &destination)
                            .await
                    }
                    MutationRequest::Copy { .. } => operator
                        .copy(source.as_deref().unwrap_or_default(), &destination)
                        .await
                        .map(|_| ()),
                    MutationRequest::PermanentDelete { .. } => operator.delete(&destination).await,
                    _ => unreachable!("unsupported mutations are validated before dispatch"),
                }
            })
            .await
            .map_err(|category| match category {
                RemoteErrorCategory::Cancelled => StoreError::Cancelled,
                _ => StoreError::Backend("remote provider runtime failed".into()),
            })?;
            cancellation.check()?;
            result.map_err(|error| OpendalStore::store_error(&error, RemoteErrorContext::Mutation))
        })
    }
}

fn mutation_paths(
    store: &OpendalStore,
    request: &MutationRequest,
) -> Result<(Option<String>, String), StoreError> {
    let source = request
        .source()
        .map(|path| store.remote_path(path))
        .transpose()
        .map_err(|_| StoreError::Backend("mutation source belongs to another provider".into()))?;
    let destination = store
        .remote_path(request.destination())
        .map_err(|_| StoreError::Backend("mutation target belongs to another provider".into()))?;
    Ok((source, destination))
}

fn normalize_store_path(path: &str) -> Result<String, StoreError> {
    let mut normalized = path.replace('\\', "/");
    if !normalized.starts_with('/') {
        normalized.insert(0, '/');
    }
    if normalized.contains('\0') || normalized.split('/').any(|segment| segment == "..") {
        return Err(StoreError::Backend("remote path is invalid".into()));
    }
    Ok(normalized)
}

fn to_opendal_path(path: &str) -> String {
    path.trim_start_matches('/').to_owned()
}

fn directory_path(path: &str) -> String {
    if path.is_empty() || path.ends_with('/') {
        path.to_owned()
    } else {
        format!("{path}/")
    }
}

fn encode_session(id: u64) -> Continuation {
    Continuation::new(id.to_be_bytes().to_vec()).expect("a session cursor is bounded")
}

fn decode_session(continuation: &Continuation) -> Result<u64, StoreError> {
    continuation
        .as_bytes()
        .try_into()
        .map(u64::from_be_bytes)
        .map_err(|_| StoreError::InvalidContinuation)
}

fn runtime() -> Result<&'static tokio::runtime::Runtime, RemoteErrorCategory> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, ()>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .thread_name("musheen-opendal")
                .build()
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|_| RemoteErrorCategory::Unavailable)
}

pub(crate) async fn run_remote<F, T>(
    cancellation: CancellationToken,
    operation: F,
) -> Result<T, RemoteErrorCategory>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    run_remote_streaming(cancellation, async move {
        tokio::time::timeout(REQUEST_TIMEOUT, operation)
            .await
            .map_err(|_| RemoteErrorCategory::Timeout)
    })
    .await?
}

async fn run_remote_streaming<F, T>(
    cancellation: CancellationToken,
    operation: F,
) -> Result<T, RemoteErrorCategory>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    if cancellation.is_cancelled() {
        return Err(RemoteErrorCategory::Cancelled);
    }
    let handle = runtime()?.spawn(operation);
    let abort = handle.abort_handle();
    let result = future::race(
        async move { handle.await.map_err(|_| RemoteErrorCategory::Unavailable) },
        async move {
            future::poll_fn(|context| {
                if cancellation.is_cancelled() {
                    std::task::Poll::Ready(())
                } else {
                    cancellation.register_waker(context.waker());
                    std::task::Poll::Pending
                }
            })
            .await;
            Err(RemoteErrorCategory::Cancelled)
        },
    )
    .await;
    if matches!(result, Err(RemoteErrorCategory::Cancelled)) {
        abort.abort();
    }
    result
}

async fn run_pooled_stream<F, Fut, T>(
    operator: Operator,
    pool: Option<ProviderPool<OperatorConnector>>,
    protocol: RemoteProtocol,
    cancellation: CancellationToken,
    operation: F,
) -> Result<T, RemoteError>
where
    F: FnOnce(Operator) -> Fut + Send,
    Fut: Future<Output = Result<T, RemoteErrorCategory>> + Send + 'static,
    T: Send + 'static,
{
    let lease = match pool {
        Some(pool) => Some(
            pool.acquire(cancellation.clone())
                .await
                .map_err(|error| RemoteError::new(protocol, error.category(), None))?,
        ),
        None => None,
    };
    let operator = lease
        .as_ref()
        .map_or(operator, |lease| lease.connection().clone());
    run_remote_streaming(cancellation, operation(operator))
        .await
        .map_err(|category| RemoteError::new(protocol, category, None))?
        .map_err(|category| RemoteError::new(protocol, category, None))
}

async fn upload_staging(
    operator: Operator,
    remote_path: &str,
    local: &Path,
) -> Result<u64, RemoteErrorCategory> {
    let mut file = tokio::fs::File::open(local)
        .await
        .map_err(|_| RemoteErrorCategory::Permanent)?;
    match tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, operator.stat(remote_path)).await {
        Ok(Ok(_)) => return Err(RemoteErrorCategory::Conflict),
        Ok(Err(error)) if error.kind() == ErrorKind::NotFound => {}
        Ok(Err(error)) => {
            return Err(classify_opendal_error(&error, RemoteErrorContext::Read));
        }
        Err(_) => return Err(RemoteErrorCategory::Timeout),
    }
    let exclusive = operator.info().capability().write_with_if_not_exists;
    let mut writer = tokio::time::timeout(
        TRANSFER_IDLE_TIMEOUT,
        operator.writer_with(remote_path).if_not_exists(exclusive),
    )
    .await
    .map_err(|_| RemoteErrorCategory::Timeout)?
    .map_err(|error| classify_opendal_error(&error, RemoteErrorContext::Mutation))?;
    let transfer = async {
        let mut buffer = vec![0; TRANSFER_CHUNK_BYTES];
        let mut written = 0_u64;
        loop {
            let count = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, file.read(&mut buffer))
                .await
                .map_err(|_| RemoteErrorCategory::Timeout)?
                .map_err(|_| RemoteErrorCategory::Permanent)?;
            if count == 0 {
                break;
            }
            tokio::time::timeout(
                TRANSFER_IDLE_TIMEOUT,
                writer.write(buffer[..count].to_vec()),
            )
            .await
            .map_err(|_| RemoteErrorCategory::Timeout)?
            .map_err(|error| classify_opendal_error(&error, RemoteErrorContext::Mutation))?;
            written = written
                .checked_add(count as u64)
                .ok_or(RemoteErrorCategory::Permanent)?;
        }
        tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, writer.close())
            .await
            .map_err(|_| RemoteErrorCategory::Timeout)?
            .map_err(|error| classify_opendal_error(&error, RemoteErrorContext::Mutation))?;
        Ok(written)
    }
    .await;
    if transfer.is_err() {
        let _ = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, writer.abort()).await;
    }
    transfer
}

async fn publish_staging_new(
    operator: Operator,
    staging: &str,
    destination: &str,
) -> Result<u64, RemoteErrorCategory> {
    let metadata = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, operator.stat(staging))
        .await
        .map_err(|_| RemoteErrorCategory::Timeout)?
        .map_err(|error| classify_opendal_error(&error, RemoteErrorContext::Read))?;
    if !metadata.is_file() {
        return Err(RemoteErrorCategory::Unsupported);
    }
    match tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, operator.stat(destination)).await {
        Ok(Ok(_)) => return Err(RemoteErrorCategory::Conflict),
        Ok(Err(error)) if error.kind() == ErrorKind::NotFound => {}
        Ok(Err(error)) => {
            return Err(classify_opendal_error(&error, RemoteErrorContext::Read));
        }
        Err(_) => return Err(RemoteErrorCategory::Timeout),
    }
    let reader = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, operator.reader(staging))
        .await
        .map_err(|_| RemoteErrorCategory::Timeout)?
        .map_err(|error| classify_opendal_error(&error, RemoteErrorContext::Read))?;
    let mut writer = tokio::time::timeout(
        TRANSFER_IDLE_TIMEOUT,
        operator.writer_with(destination).if_not_exists(true),
    )
    .await
    .map_err(|_| RemoteErrorCategory::Timeout)?
    .map_err(|error| classify_opendal_error(&error, RemoteErrorContext::Mutation))?;
    let transfer = async {
        let mut offset = 0_u64;
        while offset < metadata.content_length() {
            let end = offset
                .saturating_add(TRANSFER_CHUNK_BYTES as u64)
                .min(metadata.content_length());
            let chunk = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, reader.read(offset..end))
                .await
                .map_err(|_| RemoteErrorCategory::Timeout)?
                .map_err(|error| classify_opendal_error(&error, RemoteErrorContext::Read))?;
            if chunk.len() as u64 != end - offset {
                return Err(RemoteErrorCategory::Protocol);
            }
            tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, writer.write(chunk.to_vec()))
                .await
                .map_err(|_| RemoteErrorCategory::Timeout)?
                .map_err(|error| classify_opendal_error(&error, RemoteErrorContext::Mutation))?;
            offset = end;
        }
        let after = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, operator.stat(staging))
            .await
            .map_err(|_| RemoteErrorCategory::Timeout)?
            .map_err(|error| classify_opendal_error(&error, RemoteErrorContext::Read))?;
        if after.content_length() != metadata.content_length()
            || after.etag() != metadata.etag()
            || after.version() != metadata.version()
            || after.last_modified() != metadata.last_modified()
        {
            return Err(RemoteErrorCategory::Conflict);
        }
        tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, writer.close())
            .await
            .map_err(|_| RemoteErrorCategory::Timeout)?
            .map_err(|error| classify_opendal_error(&error, RemoteErrorContext::Mutation))?;
        Ok(offset)
    }
    .await;
    if transfer.is_err() {
        let _ = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, writer.abort()).await;
    }
    transfer
}

async fn download_new_local(
    operator: Operator,
    remote_path: &str,
    local: &Path,
    max_bytes: u64,
) -> Result<u64, RemoteErrorCategory> {
    let metadata = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, operator.stat(remote_path))
        .await
        .map_err(|_| RemoteErrorCategory::Timeout)?
        .map_err(|error| classify_opendal_error(&error, RemoteErrorContext::Read))?;
    if !metadata.is_file() {
        return Err(RemoteErrorCategory::Unsupported);
    }
    let size = metadata.content_length();
    if size > max_bytes {
        return Err(RemoteErrorCategory::Quota);
    }
    let reader = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, operator.reader(remote_path))
        .await
        .map_err(|_| RemoteErrorCategory::Timeout)?
        .map_err(|error| classify_opendal_error(&error, RemoteErrorContext::Read))?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(local)
        .await
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                RemoteErrorCategory::Conflict
            } else {
                RemoteErrorCategory::Permanent
            }
        })?;
    let mut offset = 0_u64;
    while offset < size {
        let end = offset.saturating_add(TRANSFER_CHUNK_BYTES as u64).min(size);
        let chunk = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, reader.read(offset..end))
            .await
            .map_err(|_| RemoteErrorCategory::Timeout)?
            .map_err(|error| classify_opendal_error(&error, RemoteErrorContext::Read))?;
        let bytes = chunk.to_vec();
        if bytes.len() as u64 != end - offset {
            return Err(RemoteErrorCategory::Protocol);
        }
        tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, file.write_all(&bytes))
            .await
            .map_err(|_| RemoteErrorCategory::Timeout)?
            .map_err(|_| RemoteErrorCategory::Permanent)?;
        offset = end;
    }
    tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, file.sync_all())
        .await
        .map_err(|_| RemoteErrorCategory::Timeout)?
        .map_err(|_| RemoteErrorCategory::Permanent)?;
    let after = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, operator.stat(remote_path))
        .await
        .map_err(|_| RemoteErrorCategory::Timeout)?
        .map_err(|error| classify_opendal_error(&error, RemoteErrorContext::Read))?;
    if after.content_length() != size
        || after.etag() != metadata.etag()
        || after.version() != metadata.version()
        || after.last_modified() != metadata.last_modified()
    {
        return Err(RemoteErrorCategory::Conflict);
    }
    Ok(offset)
}

fn block_on_remote<F, T>(operation: F) -> Result<T, StoreError>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    runtime()
        .map_err(|_| StoreError::Backend("remote runtime is unavailable".into()))?
        .spawn(async move {
            let _ = sender.send(operation.await);
        });
    receiver
        .recv_timeout(REQUEST_TIMEOUT)
        .map_err(|_| StoreError::Backend("remote provider request timed out".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendal::{MetadataBuilder, services::Memory};

    #[test]
    fn unknown_metadata_does_not_report_a_zero_byte_file() {
        let operator = Operator::new(Memory::default()).expect("the memory backend builds");
        let store = OpendalStore::from_operator(
            ProviderId::new("unknown-metadata").expect("the provider ID is valid"),
            RemoteProtocol::Sftp,
            operator,
            RemoteCasePolicy::Unknown,
            RemoteMutationPolicy::CapabilitiesVerified,
        )
        .expect("the adapter builds");
        let metadata = MetadataBuilder::unknown().build();

        let item = store
            .item("entry".to_owned(), &metadata)
            .expect("the unknown item is converted");

        assert_eq!(item.kind(), ItemKind::Other);
        assert_eq!(item.size(), None);
    }
}
