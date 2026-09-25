use musheen_core::{BoxFuture, CancellationToken};
use secret_service::{EncryptionType, SecretService};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use zeroize::Zeroize;

const APPLICATION_ATTRIBUTE: &str = "application";
const APPLICATION_NAME: &str = "org.musheen.Musheen";
const CONNECTION_ATTRIBUTE: &str = "connection-id";
const CONTENT_TYPE: &str = "application/octet-stream";
const SETTING_PREFIX: &str = "secret-service:";
const SERVICE_NAME: &str = "org.freedesktop.secrets";

#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ConnectionId(Box<str>);

impl ConnectionId {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, SecretError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte));
        valid
            .then_some(Self(value))
            .ok_or(SecretError::InvalidReference)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ConnectionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ConnectionId")
            .field(&self.0)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SecretPersistence {
    Persistent,
    SessionOnly,
}

#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CredentialReference {
    connection_id: ConnectionId,
    persistence: SecretPersistence,
}

impl CredentialReference {
    #[must_use]
    pub fn persistent(connection_id: ConnectionId) -> Self {
        Self {
            connection_id,
            persistence: SecretPersistence::Persistent,
        }
    }

    #[must_use]
    pub fn session_only(connection_id: ConnectionId) -> Self {
        Self {
            connection_id,
            persistence: SecretPersistence::SessionOnly,
        }
    }

    pub fn from_setting_value(value: &str) -> Result<Self, SecretError> {
        let id = value
            .strip_prefix(SETTING_PREFIX)
            .ok_or(SecretError::InvalidReference)?;
        Ok(Self::persistent(ConnectionId::new(id)?))
    }

    #[must_use]
    pub fn to_setting_value(&self) -> Option<String> {
        (self.persistence == SecretPersistence::Persistent)
            .then(|| format!("{SETTING_PREFIX}{}", self.connection_id.as_str()))
    }

    #[must_use]
    pub fn connection_id(&self) -> &ConnectionId {
        &self.connection_id
    }

    #[must_use]
    pub const fn persistence(&self) -> SecretPersistence {
        self.persistence
    }
}

impl fmt::Debug for CredentialReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialReference")
            .field("connection_id", &self.connection_id)
            .field("persistence", &self.persistence)
            .finish()
    }
}

/// App-owned plaintext storage. Its backing bytes are zeroed on clear/drop;
/// cryptographic-session memory owned by the Secret Service dependency is
/// outside this type's guarantee.
#[derive(Eq, PartialEq)]
pub struct SecretBuffer(Vec<u8>);

impl SecretBuffer {
    #[must_use]
    pub fn new(value: Vec<u8>) -> Self {
        Self(value)
    }

    pub fn expose_secret<T>(&self, operation: impl FnOnce(&[u8]) -> T) -> T {
        operation(&self.0)
    }

    pub fn clear(&mut self) {
        self.0.zeroize();
        self.0.clear();
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn duplicate(&self) -> Self {
        Self(self.0.clone())
    }

    fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for SecretBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBuffer([REDACTED])")
    }
}

impl Drop for SecretBuffer {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretServiceState {
    Available,
    Locked,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretStorage {
    Persistent,
    SessionOnlyConfirmed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretError {
    Ambiguous,
    Cancelled,
    Disconnected,
    Indeterminate,
    InvalidReference,
    Locked,
    NotFound,
    OwnerChanged,
    Protocol,
    SessionOnlyAvailable(SecretServiceState),
    Timeout,
    Unavailable,
}

impl fmt::Display for SecretError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ambiguous => formatter.write_str("multiple credentials use this reference"),
            Self::Cancelled => formatter.write_str("credential request was cancelled"),
            Self::Disconnected => formatter.write_str("credential service disconnected"),
            Self::Indeterminate => {
                formatter.write_str("credential mutation outcome is indeterminate")
            }
            Self::InvalidReference => formatter.write_str("credential reference is invalid"),
            Self::Locked => formatter.write_str("credential collection is locked"),
            Self::NotFound => formatter.write_str("credential was not found"),
            Self::OwnerChanged => formatter.write_str("credential service owner changed"),
            Self::Protocol => formatter.write_str("credential service protocol failed"),
            Self::SessionOnlyAvailable(SecretServiceState::Locked) => formatter
                .write_str("credential collection is locked; session-only use is available"),
            Self::SessionOnlyAvailable(_) => formatter
                .write_str("credential service is unavailable; session-only use is available"),
            Self::Timeout => formatter.write_str("credential service request timed out"),
            Self::Unavailable => formatter.write_str("credential service is unavailable"),
        }
    }
}

impl std::error::Error for SecretError {}

#[derive(Clone, Default)]
pub struct MutationDispatch(Arc<AtomicBool>);

impl MutationDispatch {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn mark_dispatched(&self) {
        self.0.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn was_dispatched(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub trait SecretServiceBackend: Send + Sync + 'static {
    fn state(&self) -> BoxFuture<'_, Result<SecretServiceState, SecretError>>;

    fn create<'a>(
        &'a self,
        reference: &'a CredentialReference,
        label: &'a str,
        secret: &'a SecretBuffer,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>>;

    fn read<'a>(
        &'a self,
        reference: &'a CredentialReference,
    ) -> BoxFuture<'a, Result<SecretBuffer, SecretError>>;

    fn update<'a>(
        &'a self,
        reference: &'a CredentialReference,
        label: &'a str,
        secret: &'a SecretBuffer,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>>;

    fn delete<'a>(
        &'a self,
        reference: &'a CredentialReference,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>>;

    fn rename<'a>(
        &'a self,
        reference: &'a CredentialReference,
        label: &'a str,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>>;
}

pub struct CredentialVault<B> {
    backend: B,
    session: Mutex<BTreeMap<ConnectionId, SecretBuffer>>,
}

impl<B: SecretServiceBackend> CredentialVault<B> {
    #[must_use]
    pub fn new(backend: B) -> Self {
        Self {
            backend,
            session: Mutex::new(BTreeMap::new()),
        }
    }

    pub async fn state(
        &self,
        cancellation: CancellationToken,
    ) -> Result<SecretServiceState, SecretError> {
        cancellable(self.backend.state(), cancellation).await
    }

    pub async fn create(
        &self,
        connection_id: &ConnectionId,
        label: &str,
        secret: &SecretBuffer,
        storage: SecretStorage,
        cancellation: CancellationToken,
    ) -> Result<CredentialReference, SecretError> {
        match storage {
            SecretStorage::SessionOnlyConfirmed => {
                check_cancelled(&cancellation)?;
                self.session
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(connection_id.clone(), secret.duplicate());
                Ok(CredentialReference::session_only(connection_id.clone()))
            }
            SecretStorage::Persistent => {
                let reference = CredentialReference::persistent(connection_id.clone());
                let dispatch = MutationDispatch::new();
                map_session_offer(
                    cancellable_mutation(
                        self.backend.create(&reference, label, secret, &dispatch),
                        cancellation,
                        &dispatch,
                    )
                    .await,
                )?;
                Ok(reference)
            }
        }
    }

    pub async fn read(
        &self,
        reference: &CredentialReference,
        cancellation: CancellationToken,
    ) -> Result<SecretBuffer, SecretError> {
        if reference.persistence == SecretPersistence::SessionOnly {
            check_cancelled(&cancellation)?;
            return self
                .session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(reference.connection_id())
                .map(SecretBuffer::duplicate)
                .ok_or(SecretError::NotFound);
        }
        cancellable(self.backend.read(reference), cancellation).await
    }

    pub async fn update(
        &self,
        reference: &CredentialReference,
        label: &str,
        secret: &SecretBuffer,
        cancellation: CancellationToken,
    ) -> Result<(), SecretError> {
        if reference.persistence == SecretPersistence::SessionOnly {
            check_cancelled(&cancellation)?;
            let mut session = self
                .session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let saved = session
                .get_mut(reference.connection_id())
                .ok_or(SecretError::NotFound)?;
            *saved = secret.duplicate();
            return Ok(());
        }
        let dispatch = MutationDispatch::new();
        map_session_offer(
            cancellable_mutation(
                self.backend.update(reference, label, secret, &dispatch),
                cancellation,
                &dispatch,
            )
            .await,
        )
    }

    pub async fn delete(
        &self,
        reference: &CredentialReference,
        cancellation: CancellationToken,
    ) -> Result<(), SecretError> {
        if reference.persistence == SecretPersistence::SessionOnly {
            check_cancelled(&cancellation)?;
            self.session
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(reference.connection_id());
            return Ok(());
        }
        let dispatch = MutationDispatch::new();
        cancellable_mutation(
            self.backend.delete(reference, &dispatch),
            cancellation,
            &dispatch,
        )
        .await
    }

    pub async fn rename(
        &self,
        reference: &CredentialReference,
        label: &str,
        cancellation: CancellationToken,
    ) -> Result<(), SecretError> {
        if reference.persistence == SecretPersistence::SessionOnly {
            return check_cancelled(&cancellation);
        }
        let dispatch = MutationDispatch::new();
        cancellable_mutation(
            self.backend.rename(reference, label, &dispatch),
            cancellation,
            &dispatch,
        )
        .await
    }
}

fn map_session_offer(result: Result<(), SecretError>) -> Result<(), SecretError> {
    match result {
        Err(SecretError::Locked) => Err(SecretError::SessionOnlyAvailable(
            SecretServiceState::Locked,
        )),
        Err(SecretError::Unavailable | SecretError::Disconnected | SecretError::Timeout) => Err(
            SecretError::SessionOnlyAvailable(SecretServiceState::Unavailable),
        ),
        other => other,
    }
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<(), SecretError> {
    if cancellation.is_cancelled() {
        Err(SecretError::Cancelled)
    } else {
        Ok(())
    }
}

async fn cancellable<T>(
    request: impl Future<Output = Result<T, SecretError>>,
    cancellation: CancellationToken,
) -> Result<T, SecretError> {
    if cancellation.is_cancelled() {
        return Err(SecretError::Cancelled);
    }
    futures_lite::future::race(request, async move {
        Cancelled(cancellation).await;
        Err(SecretError::Cancelled)
    })
    .await
}

async fn cancellable_mutation<T>(
    request: impl Future<Output = Result<T, SecretError>>,
    cancellation: CancellationToken,
    dispatch: &MutationDispatch,
) -> Result<T, SecretError> {
    if cancellation.is_cancelled() {
        return Err(SecretError::Cancelled);
    }
    futures_lite::future::race(request, async move {
        Cancelled(cancellation).await;
        if dispatch.was_dispatched() {
            Err(SecretError::Indeterminate)
        } else {
            Err(SecretError::Cancelled)
        }
    })
    .await
}

struct Cancelled(CancellationToken);

impl Future for Cancelled {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if self.0.is_cancelled() {
            Poll::Ready(())
        } else {
            self.0.register_waker(context.waker());
            if self.0.is_cancelled() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }
}

pub trait SecretConnectionFactory: Send + Sync + 'static {
    fn connect(&self) -> BoxFuture<'_, Result<zbus::Connection, SecretError>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SessionBusSecretConnection;

impl SecretConnectionFactory for SessionBusSecretConnection {
    fn connect(&self) -> BoxFuture<'_, Result<zbus::Connection, SecretError>> {
        Box::pin(async {
            zbus::Connection::session()
                .await
                .map_err(|_| SecretError::Unavailable)
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretEncryption {
    Dh,
    Plain,
}

impl SecretEncryption {
    fn library_type(self) -> EncryptionType {
        match self {
            Self::Dh => EncryptionType::Dh,
            Self::Plain => EncryptionType::Plain,
        }
    }
}

pub struct LinuxSecretService<C = SessionBusSecretConnection> {
    connection: C,
    encryption: SecretEncryption,
    timeout: Duration,
}

impl Default for LinuxSecretService<SessionBusSecretConnection> {
    fn default() -> Self {
        Self {
            connection: SessionBusSecretConnection,
            encryption: SecretEncryption::Dh,
            timeout: Duration::from_secs(3),
        }
    }
}

impl<C: SecretConnectionFactory> LinuxSecretService<C> {
    #[must_use]
    pub fn with_connection(connection: C, encryption: SecretEncryption) -> Self {
        Self {
            connection,
            encryption,
            timeout: Duration::from_secs(3),
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    async fn service(&self) -> Result<BoundSecretService, SecretError> {
        let connection = self.connection.connect().await?;
        let owner = service_owner(&connection).await?;
        let service: SecretService<'static> = SecretService::connect_with_existing(
            self.encryption.library_type(),
            connection.clone(),
        )
        .await
        .map_err(map_library_error)?;
        let bound = BoundSecretService {
            connection,
            owner,
            service,
        };
        bound.validate_owner().await?;
        Ok(bound)
    }
}

struct BoundSecretService {
    connection: zbus::Connection,
    owner: String,
    service: SecretService<'static>,
}

impl BoundSecretService {
    async fn validate_owner(&self) -> Result<(), SecretError> {
        if service_owner(&self.connection).await? == self.owner {
            Ok(())
        } else {
            Err(SecretError::OwnerChanged)
        }
    }
}

impl<C: SecretConnectionFactory> SecretServiceBackend for LinuxSecretService<C> {
    fn state(&self) -> BoxFuture<'_, Result<SecretServiceState, SecretError>> {
        Box::pin(async move {
            let request = async {
                let bound = self.service().await?;
                let collection = bound
                    .service
                    .get_default_collection()
                    .await
                    .map_err(map_collection_error)?;
                let locked = collection.is_locked().await.map_err(map_library_error)?;
                bound.validate_owner().await?;
                if locked {
                    Ok(SecretServiceState::Locked)
                } else {
                    Ok(SecretServiceState::Available)
                }
            };
            match with_timeout(request, self.timeout).await {
                Err(SecretError::Timeout) => Ok(SecretServiceState::Unavailable),
                Ok(state) => Ok(state),
                Err(SecretError::Unavailable | SecretError::Disconnected) => {
                    Ok(SecretServiceState::Unavailable)
                }
                Err(error) => Err(error),
            }
        })
    }

    fn create<'a>(
        &'a self,
        reference: &'a CredentialReference,
        label: &'a str,
        secret: &'a SecretBuffer,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        Box::pin(async move {
            let bound = with_timeout(self.service(), self.timeout).await?;
            let results = with_timeout(
                async {
                    bound
                        .service
                        .search_items(attributes(reference))
                        .await
                        .map_err(map_library_error)
                },
                self.timeout,
            )
            .await?;
            ensure_unique(&results)?;
            with_timeout(bound.validate_owner(), self.timeout).await?;
            if !results.locked.is_empty() {
                return Err(SecretError::Locked);
            }
            if let Some(item) = results.unlocked.first() {
                bounded_mutation(item.set_label(label), self.timeout, dispatch).await?;
                with_timeout(bound.validate_owner(), self.timeout)
                    .await
                    .map_err(|_| SecretError::Indeterminate)?;
                bounded_mutation(
                    item.set_secret(secret.as_slice(), CONTENT_TYPE),
                    self.timeout,
                    dispatch,
                )
                .await?;
            } else {
                let collection =
                    with_timeout(unlocked_collection(&bound.service), self.timeout).await?;
                bounded_mutation(
                    collection.create_item(
                        label,
                        attributes(reference),
                        secret.as_slice(),
                        true,
                        CONTENT_TYPE,
                    ),
                    self.timeout,
                    dispatch,
                )
                .await?;
            }
            with_timeout(bound.validate_owner(), self.timeout)
                .await
                .map_err(|_| SecretError::Indeterminate)
        })
    }

    fn read<'a>(
        &'a self,
        reference: &'a CredentialReference,
    ) -> BoxFuture<'a, Result<SecretBuffer, SecretError>> {
        Box::pin(async move {
            with_timeout(
                async {
                    let bound = self.service().await?;
                    let results = bound
                        .service
                        .search_items(attributes(reference))
                        .await
                        .map_err(map_library_error)?;
                    ensure_unique(&results)?;
                    bound.validate_owner().await?;
                    if let Some(item) = results.unlocked.first() {
                        let secret = item
                            .get_secret()
                            .await
                            .map(SecretBuffer::new)
                            .map_err(map_library_error)?;
                        bound.validate_owner().await?;
                        return Ok(secret);
                    }
                    if results.locked.is_empty() {
                        Err(SecretError::NotFound)
                    } else {
                        Err(SecretError::Locked)
                    }
                },
                self.timeout,
            )
            .await
        })
    }

    fn update<'a>(
        &'a self,
        reference: &'a CredentialReference,
        label: &'a str,
        secret: &'a SecretBuffer,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        Box::pin(async move {
            let bound = with_timeout(self.service(), self.timeout).await?;
            let results = with_timeout(
                async {
                    bound
                        .service
                        .search_items(attributes(reference))
                        .await
                        .map_err(map_library_error)
                },
                self.timeout,
            )
            .await?;
            ensure_unique(&results)?;
            with_timeout(bound.validate_owner(), self.timeout).await?;
            if !results.locked.is_empty() {
                return Err(SecretError::Locked);
            }
            let Some(item) = results.unlocked.first() else {
                let collection =
                    with_timeout(unlocked_collection(&bound.service), self.timeout).await?;
                bounded_mutation(
                    collection.create_item(
                        label,
                        attributes(reference),
                        secret.as_slice(),
                        true,
                        CONTENT_TYPE,
                    ),
                    self.timeout,
                    dispatch,
                )
                .await?;
                return with_timeout(bound.validate_owner(), self.timeout)
                    .await
                    .map_err(|_| SecretError::Indeterminate);
            };
            bounded_mutation(item.set_label(label), self.timeout, dispatch).await?;
            with_timeout(bound.validate_owner(), self.timeout)
                .await
                .map_err(|_| SecretError::Indeterminate)?;
            bounded_mutation(
                item.set_secret(secret.as_slice(), CONTENT_TYPE),
                self.timeout,
                dispatch,
            )
            .await?;
            with_timeout(bound.validate_owner(), self.timeout)
                .await
                .map_err(|_| SecretError::Indeterminate)
        })
    }

    fn delete<'a>(
        &'a self,
        reference: &'a CredentialReference,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        Box::pin(async move {
            let bound = with_timeout(self.service(), self.timeout).await?;
            let results = with_timeout(
                async {
                    bound
                        .service
                        .search_items(attributes(reference))
                        .await
                        .map_err(map_library_error)
                },
                self.timeout,
            )
            .await?;
            ensure_unique(&results)?;
            with_timeout(bound.validate_owner(), self.timeout).await?;
            if !results.locked.is_empty() {
                return Err(SecretError::Locked);
            }
            if let Some(item) = results.unlocked.first() {
                bounded_mutation(item.delete(), self.timeout, dispatch).await?;
                with_timeout(bound.validate_owner(), self.timeout)
                    .await
                    .map_err(|_| SecretError::Indeterminate)?;
            }
            Ok(())
        })
    }

    fn rename<'a>(
        &'a self,
        reference: &'a CredentialReference,
        label: &'a str,
        dispatch: &'a MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        Box::pin(async move {
            let bound = with_timeout(self.service(), self.timeout).await?;
            let results = with_timeout(
                async {
                    bound
                        .service
                        .search_items(attributes(reference))
                        .await
                        .map_err(map_library_error)
                },
                self.timeout,
            )
            .await?;
            ensure_unique(&results)?;
            with_timeout(bound.validate_owner(), self.timeout).await?;
            if !results.locked.is_empty() {
                return Err(SecretError::Locked);
            }
            let item = results.unlocked.first().ok_or(SecretError::NotFound)?;
            bounded_mutation(item.set_label(label), self.timeout, dispatch).await?;
            with_timeout(bound.validate_owner(), self.timeout)
                .await
                .map_err(|_| SecretError::Indeterminate)
        })
    }
}

fn attributes(reference: &CredentialReference) -> HashMap<&str, &str> {
    HashMap::from([
        (APPLICATION_ATTRIBUTE, APPLICATION_NAME),
        (CONNECTION_ATTRIBUTE, reference.connection_id.as_str()),
    ])
}

async fn unlocked_collection<'a>(
    service: &'a SecretService<'a>,
) -> Result<secret_service::Collection<'a>, SecretError> {
    let collection = service
        .get_default_collection()
        .await
        .map_err(map_collection_error)?;
    collection
        .ensure_unlocked()
        .await
        .map_err(map_library_error)?;
    Ok(collection)
}

fn ensure_unique<T>(results: &secret_service::SearchItemsResult<T>) -> Result<(), SecretError> {
    if results.unlocked.len() + results.locked.len() > 1 {
        Err(SecretError::Ambiguous)
    } else {
        Ok(())
    }
}

async fn service_owner(connection: &zbus::Connection) -> Result<String, SecretError> {
    let proxy = zbus::fdo::DBusProxy::new(connection)
        .await
        .map_err(|_| SecretError::Unavailable)?;
    let name = zbus::names::BusName::try_from(SERVICE_NAME).map_err(|_| SecretError::Protocol)?;
    proxy
        .get_name_owner(name)
        .await
        .map(|owner| owner.to_string())
        .map_err(|_| SecretError::Unavailable)
}

fn map_collection_error(error: secret_service::Error) -> SecretError {
    if matches!(error, secret_service::Error::NoResult) {
        SecretError::Unavailable
    } else {
        map_library_error(error)
    }
}

fn map_library_error(error: secret_service::Error) -> SecretError {
    match error {
        secret_service::Error::Locked => SecretError::Locked,
        secret_service::Error::NoResult => SecretError::NotFound,
        secret_service::Error::Prompt => SecretError::Cancelled,
        secret_service::Error::PromptDisconnected => SecretError::Disconnected,
        secret_service::Error::Unavailable => SecretError::Unavailable,
        secret_service::Error::Zbus(_) => SecretError::Disconnected,
        secret_service::Error::ZbusFdo(_) => SecretError::Unavailable,
        secret_service::Error::Crypto(_) | secret_service::Error::Zvariant(_) => {
            SecretError::Protocol
        }
        _ => SecretError::Protocol,
    }
}

async fn with_timeout<T>(
    request: impl Future<Output = Result<T, SecretError>>,
    timeout: Duration,
) -> Result<T, SecretError> {
    futures_lite::future::race(request, async move {
        async_io::Timer::after(timeout).await;
        Err(SecretError::Timeout)
    })
    .await
}

async fn bounded_mutation<T>(
    request: impl Future<Output = Result<T, secret_service::Error>>,
    timeout: Duration,
    dispatch: &MutationDispatch,
) -> Result<T, SecretError> {
    dispatch.mark_dispatched();
    match futures_lite::future::race(request, async move {
        async_io::Timer::after(timeout).await;
        Err(secret_service::Error::Unavailable)
    })
    .await
    {
        Ok(value) => Ok(value),
        Err(secret_service::Error::Prompt) => Err(SecretError::Cancelled),
        Err(secret_service::Error::Locked) => Err(SecretError::Locked),
        Err(_) => Err(SecretError::Indeterminate),
    }
}
