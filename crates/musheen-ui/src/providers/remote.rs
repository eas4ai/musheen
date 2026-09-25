use super::*;
use musheen_desktop::{ConnectionProfile, RemoteProtocol, SecretServiceCredentialResolver};
use std::sync::RwLock;

pub(super) type RemoteStoreConnector = Arc<
    dyn Fn(
            ProviderId,
            ConnectionProfile,
            CancellationToken,
        ) -> BoxFuture<'static, Result<Arc<dyn Store>, StoreError>>
        + Send
        + Sync,
>;

pub(super) fn default_remote_connector() -> RemoteStoreConnector {
    Arc::new(|provider, profile, cancellation| {
        Box::pin(async move {
            let credentials = SecretServiceCredentialResolver::default();
            let store: Arc<dyn Store> = match profile.protocol() {
                RemoteProtocol::Ftp | RemoteProtocol::Ftps => Arc::new(
                    musheen_desktop::ftp_store_from_profile(
                        provider,
                        &profile,
                        &credentials,
                        cancellation,
                    )
                    .await
                    .map_err(remote_error)?,
                ),
                RemoteProtocol::Sftp => Arc::new(
                    musheen_desktop::sftp_store_from_profile(
                        provider,
                        &profile,
                        &credentials,
                        cancellation,
                    )
                    .await
                    .map_err(remote_error)?,
                ),
                RemoteProtocol::WebDav => Arc::new(
                    musheen_desktop::webdav_store_from_profile(
                        provider,
                        &profile,
                        &credentials,
                        cancellation,
                    )
                    .await
                    .map_err(remote_error)?,
                ),
                RemoteProtocol::Http => Arc::new(
                    musheen_desktop::http_store_from_profile(
                        provider,
                        &profile,
                        &credentials,
                        cancellation,
                    )
                    .await
                    .map_err(remote_error)?,
                ),
                RemoteProtocol::Smb => {
                    return Err(StoreError::unsupported(
                        "open SMB connection",
                        "SMB profile connection is not yet available in this build",
                    ));
                }
                RemoteProtocol::Nfs => {
                    return Err(StoreError::unsupported(
                        "open NFS connection",
                        "NFS locations must be mounted by the system first",
                    ));
                }
            };
            Ok(store)
        })
    })
}

fn remote_error(error: musheen_desktop::RemoteError) -> StoreError {
    StoreError::Backend(error.to_string().into())
}

pub(super) struct RemoteProfileStore {
    provider: ProviderId,
    profile: ConnectionProfile,
    root: StorePath,
    connector: RemoteStoreConnector,
    connection: RwLock<Option<Arc<dyn Store>>>,
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
            .clone()
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
        if store.provider_id() != &self.provider {
            return Err(StoreError::Backend(
                "remote connector returned a different provider".into(),
            ));
        }
        *self
            .connection
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&store));
        Ok(store)
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
