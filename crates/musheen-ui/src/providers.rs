use musheen_core::{
    BoxFuture, CancellationToken, CapabilityKind, CapabilityMatrix, CapabilityReason,
    CapabilityState, DirectoryWatch, DisplayPath, ItemId, ItemKind, MutationRequest, Page,
    PageRequest, ProviderId, SearchCapabilities, SearchQuery, SearchStream, Store, StoreError,
    StoreItem, StorePath, TotalHint,
};
use musheen_desktop::{LaunchTarget, MimeDetector};
use musheen_local::{LocalOperationQueue, LocalStore, ProviderTransferRoute};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

const NETWORK_PROVIDER_ID: &str = "musheen.network";
const NETWORK_ROOT_KEY: &[u8] = b"root";

pub(crate) trait ProviderAdapter: Send + Sync {
    fn store(&self) -> Arc<dyn Store>;

    /// Converts an opaque provider key at the provider boundary. Callers must
    /// never guess that a provider key is a URI.
    fn application_target(&self, _path: &StorePath) -> Option<ProviderApplicationTarget> {
        None
    }

    fn transfer_routes(&self) -> Vec<Arc<dyn ProviderTransferRoute>> {
        Vec::new()
    }
}

#[derive(Clone)]
pub(crate) struct ProviderRuntime {
    store: Arc<RoutingStore>,
    routes: Arc<[Arc<dyn ProviderTransferRoute>]>,
    adapters: Arc<HashMap<ProviderId, Arc<dyn ProviderAdapter>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProviderApplicationTarget {
    launch_target: LaunchTarget,
    mime_type: Box<str>,
}

impl ProviderApplicationTarget {
    pub(crate) fn new(launch_target: LaunchTarget, mime_type: impl Into<Box<str>>) -> Self {
        Self {
            launch_target,
            mime_type: mime_type.into(),
        }
    }

    pub(crate) fn launch_target(&self) -> &LaunchTarget {
        &self.launch_target
    }

    pub(crate) fn mime_type(&self) -> &str {
        &self.mime_type
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApplicationTargetError {
    DetectionFailed,
    ProviderUnsupported,
}

impl fmt::Debug for ProviderRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderRuntime")
            .field(
                "providers",
                &self.store.providers.keys().collect::<Vec<_>>(),
            )
            .field("transfer_route_count", &self.routes.len())
            .finish()
    }
}

impl ProviderRuntime {
    #[must_use]
    pub(crate) fn builder() -> ProviderRuntimeBuilder {
        ProviderRuntimeBuilder::shipping()
    }

    #[must_use]
    pub(crate) fn for_current_user() -> Self {
        Self::builder()
            .build()
            .expect("the built-in provider registrations are valid")
    }

    pub(crate) fn with_primary_store(store: Arc<dyn Store>) -> Result<Self, ProviderRuntimeError> {
        let primary = ProviderId::new("local").expect("the built-in provider ID is valid");
        if store.provider_id() != &primary {
            return Err(ProviderRuntimeError::MissingPrimaryProvider(primary));
        }
        let mut builder = ProviderRuntimeBuilder::shipping();
        builder.adapters.remove(&primary);
        builder
            .register_adapter(Arc::new(StoreOnlyAdapter::new(store)))?
            .build()
    }

    pub(crate) fn with_additional_store(
        &self,
        store: Arc<dyn Store>,
    ) -> Result<Self, ProviderRuntimeError> {
        ProviderRuntimeBuilder {
            adapters: self.adapters.as_ref().clone(),
        }
        .register_adapter(Arc::new(StoreOnlyAdapter::new(store)))?
        .build()
    }

    pub(crate) fn store(&self) -> Arc<dyn Store> {
        self.store.clone()
    }

    pub(crate) fn application_target(
        &self,
        path: &StorePath,
    ) -> Result<ProviderApplicationTarget, ApplicationTargetError> {
        if let Some(path) = path.as_unix_path() {
            let detected = MimeDetector::default()
                .detect(path)
                .map_err(|_| ApplicationTargetError::DetectionFailed)?;
            return Ok(ProviderApplicationTarget::new(
                LaunchTarget::local(path.to_path_buf()),
                detected.mime_type(),
            ));
        }
        let provider = path
            .provider_key()
            .map(|(provider, _)| provider)
            .ok_or(ApplicationTargetError::ProviderUnsupported)?;
        self.adapters
            .get(provider)
            .and_then(|adapter| adapter.application_target(path))
            .ok_or(ApplicationTargetError::ProviderUnsupported)
    }

    /// Returns false only when the owning provider authoritatively reports
    /// that a saved location is absent. Offline and inaccessible providers
    /// remain restorable so the live surface can present their actual state.
    #[must_use]
    pub(crate) fn location_exists(&self, location: &StorePath) -> bool {
        match self.store.resolve_item(location) {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(_) => true,
        }
    }

    pub(crate) fn configure_queue(&self, queue: &mut LocalOperationQueue) {
        for route in self.routes.iter() {
            queue.register_provider_transfer_route(Arc::clone(route));
        }
    }
}

pub(crate) struct ProviderRuntimeBuilder {
    adapters: HashMap<ProviderId, Arc<dyn ProviderAdapter>>,
}

impl ProviderRuntimeBuilder {
    fn shipping() -> Self {
        let builder = Self {
            adapters: HashMap::new(),
        };
        builder
            .register_adapter(Arc::new(StoreOnlyAdapter::new(Arc::new(LocalStore::new()))))
            .and_then(|builder| {
                builder.register_adapter(Arc::new(StoreOnlyAdapter::new(Arc::new(
                    NetworkDiscoveryStore::new(),
                ))))
            })
            .expect("the built-in provider registrations are unique")
    }

    pub(crate) fn register_adapter(
        mut self,
        adapter: Arc<dyn ProviderAdapter>,
    ) -> Result<Self, ProviderRuntimeError> {
        self.insert_adapter(adapter)?;
        Ok(self)
    }

    fn insert_adapter(
        &mut self,
        adapter: Arc<dyn ProviderAdapter>,
    ) -> Result<(), ProviderRuntimeError> {
        let provider = adapter.store().provider_id().clone();
        if self.adapters.insert(provider.clone(), adapter).is_some() {
            return Err(ProviderRuntimeError::DuplicateProvider(provider));
        }
        Ok(())
    }

    pub(crate) fn build(self) -> Result<ProviderRuntime, ProviderRuntimeError> {
        let primary = ProviderId::new("local").expect("the built-in local provider ID is valid");
        if !self.adapters.contains_key(&primary) {
            return Err(ProviderRuntimeError::MissingPrimaryProvider(primary));
        }
        let mut providers = HashMap::with_capacity(self.adapters.len());
        let mut routes = Vec::new();
        let mut route_pairs = HashMap::new();
        for (provider, adapter) in &self.adapters {
            providers.insert(provider.clone(), adapter.store());
            for route in adapter.transfer_routes() {
                let pair = (
                    route.source_provider_id().clone(),
                    route.destination_provider_id().clone(),
                );
                if route_pairs.insert(pair.clone(), ()).is_some() {
                    return Err(ProviderRuntimeError::DuplicateTransferRoute(pair));
                }
                routes.push(route);
            }
        }
        Ok(ProviderRuntime {
            store: Arc::new(RoutingStore { primary, providers }),
            routes: routes.into(),
            adapters: Arc::new(self.adapters),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProviderRuntimeError {
    DuplicateProvider(ProviderId),
    DuplicateTransferRoute((ProviderId, ProviderId)),
    MissingPrimaryProvider(ProviderId),
}

impl fmt::Display for ProviderRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateProvider(provider) => {
                write!(
                    formatter,
                    "provider {} is already registered",
                    provider.as_str()
                )
            }
            Self::DuplicateTransferRoute((source, destination)) => write!(
                formatter,
                "transfer route {} to {} is already registered",
                source.as_str(),
                destination.as_str()
            ),
            Self::MissingPrimaryProvider(provider) => write!(
                formatter,
                "primary provider {} is not registered",
                provider.as_str()
            ),
        }
    }
}

impl std::error::Error for ProviderRuntimeError {}

struct StoreOnlyAdapter {
    store: Arc<dyn Store>,
}

impl StoreOnlyAdapter {
    fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
}

impl ProviderAdapter for StoreOnlyAdapter {
    fn store(&self) -> Arc<dyn Store> {
        Arc::clone(&self.store)
    }
}

struct RoutingStore {
    primary: ProviderId,
    providers: HashMap<ProviderId, Arc<dyn Store>>,
}

impl RoutingStore {
    fn provider_for_path(&self, path: &StorePath) -> Result<Arc<dyn Store>, StoreError> {
        let provider = path
            .provider_key()
            .map_or(&self.primary, |(provider, _)| provider);
        self.providers.get(provider).cloned().ok_or_else(|| {
            StoreError::Backend(format!("provider {} is not registered", provider.as_str()).into())
        })
    }

    fn provider_for_mutation(
        &self,
        request: &MutationRequest,
    ) -> Result<Arc<dyn Store>, StoreError> {
        let destination = self.provider_for_path(request.destination())?;
        if let Some(source) = request.source() {
            let source_provider = self.provider_for_path(source)?;
            if source_provider.provider_id() != destination.provider_id() {
                return Err(StoreError::unsupported(
                    request.kind().as_str(),
                    "cross-provider mutations must use the operation transfer router",
                ));
            }
        }
        Ok(destination)
    }
}

impl Store for RoutingStore {
    fn provider_id(&self) -> &ProviderId {
        &self.primary
    }

    fn capabilities(&self, location: &StorePath) -> CapabilityMatrix {
        self.provider_for_path(location).map_or_else(
            |_| {
                CapabilityMatrix::new(|_| {
                    CapabilityState::Unsupported(
                        CapabilityReason::new("the provider is not registered")
                            .expect("the missing-provider reason is valid"),
                    )
                })
            },
            |provider| provider.capabilities(location),
        )
    }

    fn resolve_item(&self, path: &StorePath) -> Result<Option<StoreItem>, StoreError> {
        self.provider_for_path(path)?.resolve_item(path)
    }

    fn location_writable(&self, path: &StorePath) -> Result<CapabilityState, StoreError> {
        self.provider_for_path(path)?.location_writable(path)
    }

    fn executable_state(&self, path: &StorePath) -> Result<CapabilityState, StoreError> {
        self.provider_for_path(path)?.executable_state(path)
    }

    fn search_capabilities(&self, location: &StorePath) -> SearchCapabilities {
        self.provider_for_path(location).map_or_else(
            |_| SearchCapabilities::default(),
            |provider| provider.search_capabilities(location),
        )
    }

    fn search<'a>(
        &'a self,
        scope: &'a StorePath,
        query: SearchQuery,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn SearchStream>, StoreError>> {
        let provider = self.provider_for_path(scope);
        let scope = scope.clone();
        Box::pin(async move { provider?.search(&scope, query, cancellation).await })
    }

    fn read_directory<'a>(
        &'a self,
        location: &'a StorePath,
        request: PageRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Page<StoreItem>, StoreError>> {
        let provider = self.provider_for_path(location);
        let location = location.clone();
        Box::pin(async move {
            provider?
                .read_directory(&location, request, cancellation)
                .await
        })
    }

    fn watch_directory<'a>(
        &'a self,
        location: &'a StorePath,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn DirectoryWatch>, StoreError>> {
        let provider = self.provider_for_path(location);
        let location = location.clone();
        Box::pin(async move { provider?.watch_directory(&location, cancellation).await })
    }

    fn validate_mutation(&self, request: &MutationRequest) -> Result<(), StoreError> {
        self.provider_for_mutation(request)?
            .validate_mutation(request)
    }

    fn mutate<'a>(
        &'a self,
        request: MutationRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        let provider = self.provider_for_mutation(&request);
        Box::pin(async move { provider?.mutate(request, cancellation).await })
    }
}

struct NetworkDiscoveryStore {
    provider: ProviderId,
    root: StorePath,
}

impl NetworkDiscoveryStore {
    fn new() -> Self {
        let provider = ProviderId::new(NETWORK_PROVIDER_ID)
            .expect("the built-in network provider ID is valid");
        let root = StorePath::from_provider_key(provider.clone(), NETWORK_ROOT_KEY.to_vec())
            .expect("the built-in network root key is valid");
        Self { provider, root }
    }

    fn root_item(&self) -> StoreItem {
        StoreItem::new(
            ItemId::new(self.provider.clone(), NETWORK_ROOT_KEY.to_vec())
                .expect("the built-in network root identity is valid"),
            self.root.clone(),
            DisplayPath::new("Network"),
            ItemKind::Directory,
            None,
        )
    }
}

impl Store for NetworkDiscoveryStore {
    fn provider_id(&self) -> &ProviderId {
        &self.provider
    }

    fn capabilities(&self, _location: &StorePath) -> CapabilityMatrix {
        CapabilityMatrix::new(|kind| match kind {
            CapabilityKind::CaseSensitivity => CapabilityState::Supported,
            _ => CapabilityState::Unsupported(
                CapabilityReason::new("network discovery locations are read-only")
                    .expect("the network capability reason is valid"),
            ),
        })
    }

    fn resolve_item(&self, path: &StorePath) -> Result<Option<StoreItem>, StoreError> {
        Ok((path == &self.root).then(|| self.root_item()))
    }

    fn location_writable(&self, _path: &StorePath) -> Result<CapabilityState, StoreError> {
        Ok(CapabilityState::Unsupported(
            CapabilityReason::new("network discovery locations are read-only")
                .expect("the network writable reason is valid"),
        ))
    }

    fn read_directory<'a>(
        &'a self,
        location: &'a StorePath,
        request: PageRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Page<StoreItem>, StoreError>> {
        let is_root = location == &self.root;
        Box::pin(async move {
            cancellation.check()?;
            if !is_root {
                return Err(StoreError::Backend(
                    "network location does not exist".into(),
                ));
            }
            Page::try_new(&request, Vec::new(), None, TotalHint::Exact(0))
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
                "network discovery is refreshed manually",
            ))
        })
    }

    fn validate_mutation(&self, request: &MutationRequest) -> Result<(), StoreError> {
        Err(request.unsupported("network discovery locations are read-only"))
    }

    fn mutate<'a>(
        &'a self,
        request: MutationRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        let result = cancellation
            .check()
            .and_then(|()| self.validate_mutation(&request));
        Box::pin(async move { result })
    }
}

#[must_use]
pub(crate) fn network_root_path() -> StorePath {
    StorePath::from_provider_key(
        ProviderId::new(NETWORK_PROVIDER_ID).expect("the built-in network provider ID is valid"),
        NETWORK_ROOT_KEY.to_vec(),
    )
    .expect("the built-in network root key is valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use musheen_desktop::{
        ArchiveFormat, ArchiveLimits, ArchivePasswordProvider, ArchiveStore, PasswordRequest,
    };
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn additional_store_routes_archive_paths_without_replacing_local_storage() {
        let mut source = tempfile::NamedTempFile::new().unwrap();
        source
            .write_all(&[
                0x50, 0x4b, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ])
            .unwrap();
        let passwords: Arc<dyn ArchivePasswordProvider> = Arc::new(|_: &PasswordRequest| Ok(None));
        let archive = Arc::new(
            ArchiveStore::from_file(
                File::open(source.path()).unwrap(),
                "empty.zip",
                ArchiveFormat::Zip,
                passwords,
                ArchiveLimits::default(),
            )
            .unwrap(),
        );
        let root = archive.root_path();

        let runtime = ProviderRuntime::for_current_user()
            .with_additional_store(archive)
            .unwrap();

        assert_eq!(runtime.store().provider_id().as_str(), "local");
        assert_eq!(runtime.store().resolve_item(&root).unwrap(), None);
        assert!(matches!(
            runtime
                .store()
                .capabilities(&root)
                .get(CapabilityKind::Watching),
            CapabilityState::Unsupported(_)
        ));
    }
}
