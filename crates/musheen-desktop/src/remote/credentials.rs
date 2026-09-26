use super::{CredentialResolver, RemoteErrorCategory};
use crate::{
    ConnectionId, CredentialReference, CredentialVault, LinuxSecretService, SecretBuffer,
    SecretError, SecretServiceBackend, SecretServiceState, SecretStorage,
};
use musheen_core::{BoxFuture, CancellationToken};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

/// The app's one store of remote-connection secrets: the desktop secret
/// service, and the secrets a user chose to keep for this session only.
///
/// A saved profile keeps the same reference either way, so the settings file
/// never records where a secret lives. A session-only secret wins over the
/// secret service until the app exits or the connection is removed.
pub struct RemoteCredentials {
    vault: CredentialVault<Arc<dyn SecretServiceBackend>>,
    session: Mutex<BTreeMap<ConnectionId, SecretBuffer>>,
}

impl RemoteCredentials {
    #[must_use]
    pub fn new(backend: Arc<dyn SecretServiceBackend>) -> Self {
        Self {
            vault: CredentialVault::new(backend),
            session: Mutex::new(BTreeMap::new()),
        }
    }

    /// The desktop secret service on the session bus.
    #[must_use]
    pub fn system() -> Self {
        Self::new(Arc::new(LinuxSecretService::default()))
    }

    /// Whether the secret service can store a secret now, without storing
    /// one. A locked or missing service fails with
    /// `SecretError::SessionOnlyAvailable`, as `store` would.
    pub async fn availability(&self, cancellation: CancellationToken) -> Result<(), SecretError> {
        match self.vault.state(cancellation).await {
            Ok(SecretServiceState::Available) => Ok(()),
            Ok(state) => Err(SecretError::SessionOnlyAvailable(state)),
            Err(SecretError::Locked) => Err(SecretError::SessionOnlyAvailable(
                SecretServiceState::Locked,
            )),
            Err(SecretError::Unavailable | SecretError::Disconnected | SecretError::Timeout) => {
                Err(SecretError::SessionOnlyAvailable(
                    SecretServiceState::Unavailable,
                ))
            }
            Err(error) => Err(error),
        }
    }

    /// Stores `secret` under `id`, in the secret service or, when `storage`
    /// is session-only, in memory. A locked or missing secret service fails
    /// with `SecretError::SessionOnlyAvailable`, and nothing is stored.
    pub async fn store(
        &self,
        id: &ConnectionId,
        label: &str,
        secret: &SecretBuffer,
        storage: SecretStorage,
        cancellation: CancellationToken,
    ) -> Result<CredentialReference, SecretError> {
        let reference = CredentialReference::persistent(id.clone());
        match storage {
            SecretStorage::SessionOnlyConfirmed => {
                if cancellation.is_cancelled() {
                    return Err(SecretError::Cancelled);
                }
                self.sessions().insert(id.clone(), secret.duplicate());
            }
            SecretStorage::Persistent => {
                self.vault
                    .create(id, label, secret, SecretStorage::Persistent, cancellation)
                    .await?;
                self.sessions().remove(id);
            }
        }
        Ok(reference)
    }

    /// Forgets the secret stored under `id`, in memory and in the secret
    /// service. A secret that is not there counts as removed.
    pub async fn remove(
        &self,
        id: &ConnectionId,
        cancellation: CancellationToken,
    ) -> Result<(), SecretError> {
        self.sessions().remove(id);
        match self
            .vault
            .delete(&CredentialReference::persistent(id.clone()), cancellation)
            .await
        {
            Ok(()) | Err(SecretError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, BTreeMap<ConnectionId, SecretBuffer>> {
        self.session.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for RemoteCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteCredentials(..)")
    }
}

impl CredentialResolver for RemoteCredentials {
    fn resolve<'a>(
        &'a self,
        reference: &'a CredentialReference,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<SecretBuffer, RemoteErrorCategory>> {
        Box::pin(async move {
            if let Some(secret) = self.sessions().get(reference.connection_id()) {
                return Ok(secret.duplicate());
            }
            self.vault
                .read(reference, cancellation)
                .await
                .map_err(resolve_error)
        })
    }
}

impl<T: CredentialResolver + ?Sized> CredentialResolver for Arc<T> {
    fn resolve<'a>(
        &'a self,
        reference: &'a CredentialReference,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<SecretBuffer, RemoteErrorCategory>> {
        (**self).resolve(reference, cancellation)
    }
}

/// Why a stored secret could not be read: the secret service is the cause,
/// not the server, which was never asked.
pub(crate) fn resolve_error(error: SecretError) -> RemoteErrorCategory {
    match error {
        SecretError::Cancelled => RemoteErrorCategory::Cancelled,
        _ => RemoteErrorCategory::CredentialUnavailable,
    }
}

impl SecretServiceBackend for Arc<dyn SecretServiceBackend> {
    fn state(&self) -> BoxFuture<'_, Result<crate::SecretServiceState, SecretError>> {
        (**self).state()
    }

    fn create<'a>(
        &'a self,
        reference: &'a CredentialReference,
        label: &'a str,
        secret: &'a SecretBuffer,
        dispatch: &'a crate::MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        (**self).create(reference, label, secret, dispatch)
    }

    fn read<'a>(
        &'a self,
        reference: &'a CredentialReference,
    ) -> BoxFuture<'a, Result<SecretBuffer, SecretError>> {
        (**self).read(reference)
    }

    fn update<'a>(
        &'a self,
        reference: &'a CredentialReference,
        label: &'a str,
        secret: &'a SecretBuffer,
        dispatch: &'a crate::MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        (**self).update(reference, label, secret, dispatch)
    }

    fn delete<'a>(
        &'a self,
        reference: &'a CredentialReference,
        dispatch: &'a crate::MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        (**self).delete(reference, dispatch)
    }

    fn rename<'a>(
        &'a self,
        reference: &'a CredentialReference,
        label: &'a str,
        dispatch: &'a crate::MutationDispatch,
    ) -> BoxFuture<'a, Result<(), SecretError>> {
        (**self).rename(reference, label, dispatch)
    }
}
