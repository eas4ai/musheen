use super::*;
use musheen_desktop::{
    ConnectionProfile, CredentialResolver, OpendalStore, RemoteCredentials, RemoteError,
    RemoteErrorCategory, RemoteProtocol, SshEnvironment,
};
use std::sync::RwLock;

mod relay;
mod transfer;
pub(super) use relay::RemoteRelayRoute;
pub(super) use transfer::{RemoteDownloadRoute, RemoteUploadRoute};

pub(super) struct RemoteProfileAdapter {
    store: Arc<RemoteProfileStore>,
    name: Box<str>,
    peers: Vec<Arc<RemoteProfileStore>>,
}

impl RemoteProfileAdapter {
    pub(super) fn new(
        store: Arc<RemoteProfileStore>,
        name: &str,
        peers: Vec<Arc<RemoteProfileStore>>,
    ) -> Self {
        Self {
            store,
            name: name.into(),
            peers,
        }
    }
}

impl ProviderAdapter for RemoteProfileAdapter {
    fn store(&self) -> Arc<dyn Store> {
        self.store.clone()
    }

    fn network_name(&self) -> Option<&str> {
        Some(&self.name)
    }

    fn transfer_routes(&self) -> Vec<Arc<dyn ProviderTransferRoute>> {
        let mut routes: Vec<Arc<dyn ProviderTransferRoute>> = vec![
            Arc::new(RemoteUploadRoute::new(self.store.clone())),
            Arc::new(RemoteDownloadRoute::new(self.store.clone())),
        ];
        routes.extend(self.peers.iter().map(|destination| {
            Arc::new(RemoteRelayRoute::new(
                self.store.clone(),
                destination.clone(),
            )) as Arc<dyn ProviderTransferRoute>
        }));
        routes
    }
}

#[derive(Clone)]
pub(super) struct RemoteStoreConnection {
    store: Arc<dyn Store>,
    transfer: Option<Arc<OpendalStore>>,
}

impl RemoteStoreConnection {
    pub(super) fn opendal(store: Arc<OpendalStore>) -> Self {
        Self {
            store: store.clone(),
            transfer: Some(store),
        }
    }

    #[cfg(test)]
    pub(super) fn browse_only(store: Arc<dyn Store>) -> Self {
        Self {
            store,
            transfer: None,
        }
    }
}

pub(super) type RemoteStoreConnector = Arc<
    dyn Fn(
            ProviderId,
            ConnectionProfile,
            CancellationToken,
        ) -> BoxFuture<'static, Result<RemoteStoreConnection, StoreError>>
        + Send
        + Sync,
>;

pub(super) fn default_remote_connector() -> RemoteStoreConnector {
    remote_connector(
        Arc::new(RemoteCredentials::system()),
        SshEnvironment::for_current_user(),
    )
}

/// Opens saved connections with `credentials` and the SSH files `ssh` names.
pub(super) fn remote_connector(
    credentials: Arc<dyn CredentialResolver>,
    ssh: SshEnvironment,
) -> RemoteStoreConnector {
    Arc::new(move |provider, profile, cancellation| {
        let credentials = Arc::clone(&credentials);
        let ssh = ssh.clone();
        Box::pin(async move {
            open_remote_store(provider, &profile, &credentials, &ssh, cancellation)
                .await
                .map(RemoteStoreConnection::opendal)
                .map_err(remote_error)
        })
    })
}

/// Opens `profile` the way browsing does. A connection browsing cannot open
/// in this build is refused before any network I/O.
async fn open_remote_store(
    provider: ProviderId,
    profile: &ConnectionProfile,
    credentials: &Arc<dyn CredentialResolver>,
    ssh: &SshEnvironment,
    cancellation: CancellationToken,
) -> Result<Arc<OpendalStore>, RemoteError> {
    if profile.browse_refusal().is_some() {
        return Err(RemoteError::new(
            profile.protocol(),
            RemoteErrorCategory::Unsupported,
            Some(profile.host().clone()),
        ));
    }
    let store = match profile.protocol() {
        RemoteProtocol::Ftp | RemoteProtocol::Ftps => {
            musheen_desktop::ftp_store_from_profile(provider, profile, credentials, cancellation)
                .await?
        }
        RemoteProtocol::Sftp => {
            musheen_desktop::sftp_store_from_profile_in(
                provider,
                profile,
                credentials,
                ssh,
                cancellation,
            )
            .await?
        }
        RemoteProtocol::WebDav => {
            musheen_desktop::webdav_store_from_profile(provider, profile, credentials, cancellation)
                .await?
        }
        RemoteProtocol::Http => {
            musheen_desktop::http_store_from_profile(provider, profile, credentials, cancellation)
                .await?
        }
        RemoteProtocol::Smb | RemoteProtocol::Nfs => {
            return Err(RemoteError::new(
                profile.protocol(),
                RemoteErrorCategory::Unsupported,
                Some(profile.host().clone()),
            ));
        }
    };
    Ok(Arc::new(store))
}

/// Tests a connection by opening it the way browsing does and reading its
/// root, so a connection passes only when browsing can open it.
pub(crate) struct BrowseConnectionTester {
    ssh: SshEnvironment,
}

impl BrowseConnectionTester {
    pub(crate) fn new(ssh: SshEnvironment) -> Self {
        Self { ssh }
    }
}

impl crate::settings::ConnectionTestService for BrowseConnectionTester {
    fn test<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        credentials: Arc<dyn CredentialResolver>,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteError>> {
        Box::pin(async move {
            let provider = ProviderId::new("musheen.remote.connection-test")
                .expect("the connection test provider ID is valid");
            let store = open_remote_store(
                provider,
                profile,
                &credentials,
                &self.ssh,
                cancellation.clone(),
            )
            .await?;
            store.check_root(cancellation).await.map_err(|category| {
                RemoteError::new(profile.protocol(), category, Some(profile.host().clone()))
            })
        })
    }
}

fn remote_error(error: musheen_desktop::RemoteError) -> StoreError {
    StoreError::Backend(error.to_string().into())
}

pub(super) struct RemoteProfileStore {
    provider: ProviderId,
    profile: ConnectionProfile,
    root: StorePath,
    connector: RemoteStoreConnector,
    connection: RwLock<Option<RemoteStoreConnection>>,
    connect_gate: tokio::sync::Mutex<()>,
}

impl RemoteProfileStore {
    pub(super) fn new(
        profile: ConnectionProfile,
        connector: RemoteStoreConnector,
    ) -> Result<Self, StoreError> {
        let digest = blake3::hash(profile.id().as_str().as_bytes()).to_hex();
        let provider = ProviderId::new(format!("musheen.remote.{}", &digest[..32]))
            .map_err(|_| StoreError::Backend("remote profile ID is invalid".into()))?;
        let root = StorePath::from_provider_key(provider.clone(), b"/".to_vec())
            .map_err(|_| StoreError::Backend("remote profile root is invalid".into()))?;
        Ok(Self {
            provider,
            profile,
            root,
            connector,
            connection: RwLock::new(None),
            connect_gate: tokio::sync::Mutex::new(()),
        })
    }

    fn connected_store(&self) -> Option<Arc<dyn Store>> {
        self.connection
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|connection| Arc::clone(&connection.store))
    }

    async fn connect(&self, cancellation: CancellationToken) -> Result<Arc<dyn Store>, StoreError> {
        cancellation.check()?;
        if let Some(store) = self.connected_store() {
            return Ok(store);
        }
        let _guard = self.connect_gate.lock().await;
        cancellation.check()?;
        if let Some(store) = self.connected_store() {
            return Ok(store);
        }
        let store = (self.connector)(
            self.provider.clone(),
            self.profile.clone(),
            cancellation.clone(),
        )
        .await?;
        cancellation.check()?;
        if store.store.provider_id() != &self.provider {
            return Err(StoreError::Backend(
                "remote connector returned a different provider".into(),
            ));
        }
        *self
            .connection
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(store.clone());
        Ok(store.store)
    }

    pub(super) async fn connect_transfer(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Arc<OpendalStore>, StoreError> {
        self.connect(cancellation).await?;
        self.connection
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|connection| connection.transfer.clone())
            .ok_or_else(|| {
                StoreError::unsupported("transfer remote item", "connection has no transfer API")
            })
    }

    fn root_item(&self) -> StoreItem {
        provider_root_item(&self.provider, &self.root, b"/", self.profile.name())
    }
}

impl Store for RemoteProfileStore {
    fn provider_id(&self) -> &ProviderId {
        &self.provider
    }

    fn capabilities(&self, location: &StorePath) -> CapabilityMatrix {
        self.connected_store().map_or_else(
            || {
                CapabilityMatrix::new(|_| {
                    CapabilityState::Unknown(
                        CapabilityReason::new("connect to check remote capabilities")
                            .expect("the remote capability reason is valid"),
                    )
                })
            },
            |store| store.capabilities(location),
        )
    }

    fn resolve_item(&self, path: &StorePath) -> Result<Option<StoreItem>, StoreError> {
        if path == &self.root {
            return Ok(Some(self.root_item()));
        }
        self.connected_store()
            .ok_or_else(|| StoreError::Backend("remote connection is not open".into()))?
            .resolve_item(path)
    }

    fn location_writable(&self, path: &StorePath) -> Result<CapabilityState, StoreError> {
        match self.connected_store() {
            Some(store) => store.location_writable(path),
            None => Ok(CapabilityState::Unknown(
                CapabilityReason::new("connect to check remote access")
                    .expect("the remote access reason is valid"),
            )),
        }
    }

    fn read_directory<'a>(
        &'a self,
        location: &'a StorePath,
        request: PageRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Page<StoreItem>, StoreError>> {
        Box::pin(async move {
            let store = self.connect(cancellation.clone()).await?;
            store.read_directory(location, request, cancellation).await
        })
    }

    fn watch_directory<'a>(
        &'a self,
        location: &'a StorePath,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn DirectoryWatch>, StoreError>> {
        Box::pin(async move {
            let store = self.connect(cancellation.clone()).await?;
            store.watch_directory(location, cancellation).await
        })
    }

    fn validate_mutation(&self, request: &MutationRequest) -> Result<(), StoreError> {
        self.connected_store()
            .ok_or_else(|| {
                StoreError::unsupported("mutate remote item", "remote connection is not open")
            })?
            .validate_mutation(request)
    }

    fn mutate<'a>(
        &'a self,
        request: MutationRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let store = self.connect(cancellation.clone()).await?;
            store.mutate(request, cancellation).await
        })
    }
}
