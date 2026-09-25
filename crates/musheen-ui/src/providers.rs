use musheen_core::{
    BoxFuture, CancellationToken, CapabilityKind, CapabilityMatrix, CapabilityReason,
    CapabilityState, Continuation, DirectoryWatch, DisplayPath, ItemId, ItemKind, MutationRequest,
    Page, PageRequest, ProviderId, SearchCapabilities, SearchQuery, SearchStream, Store,
    StoreError, StoreItem, StorePath, TotalHint,
};
use musheen_desktop::{LaunchTarget, MimeDetector};
use musheen_local::{LocalOperationQueue, LocalStore, ProviderTransferRoute};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

const NETWORK_PROVIDER_ID: &str = "musheen.network";
const NETWORK_ROOT_KEY: &[u8] = b"root";

fn provider_root_item(
    provider: &ProviderId,
    root: &StorePath,
    identity_key: &[u8],
    name: &str,
) -> StoreItem {
    StoreItem::new(
        ItemId::new(provider.clone(), identity_key.to_vec())
            .expect("a built-in provider root identity is valid"),
        root.clone(),
        DisplayPath::new(name),
        ItemKind::Directory,
        None,
    )
}

mod remote;
use remote::{
    RemoteProfileAdapter, RemoteProfileStore, RemoteStoreConnector, default_remote_connector,
};

pub(crate) trait ProviderAdapter: Send + Sync {
    fn store(&self) -> Arc<dyn Store>;

    fn network_name(&self) -> Option<&str> {
        None
    }

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

    pub(crate) fn from_settings(
        settings: &musheen_desktop::SettingsDocument,
    ) -> Result<Self, ProviderRuntimeError> {
        Self::from_settings_with_connector(settings, default_remote_connector())
    }

    fn from_settings_with_connector(
        settings: &musheen_desktop::SettingsDocument,
        connector: RemoteStoreConnector,
    ) -> Result<Self, ProviderRuntimeError> {
        let mut builder = Self::builder();
        if let Some(document) = settings.value("remote.connections") {
            let profiles = musheen_desktop::ConnectionProfiles::import(&document)
                .map_err(|_| ProviderRuntimeError::InvalidConnectionProfiles)?;
            for profile in profiles.profiles() {
                let store = Arc::new(
                    RemoteProfileStore::new(profile.clone(), Arc::clone(&connector))
                        .map_err(|_| ProviderRuntimeError::InvalidConnectionProfiles)?,
                );
                builder = builder
                    .register_adapter(Arc::new(RemoteProfileAdapter::new(store, profile.name())))?;
            }
        }
        builder.build()
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
        let mut remote_entries = Vec::new();
        for (provider, adapter) in &self.adapters {
            providers.insert(provider.clone(), adapter.store());
            if let Some(name) = adapter.network_name() {
                let root = StorePath::from_provider_key(provider.clone(), b"/".to_vec())
                    .expect("a registered remote provider has a valid root key");
                remote_entries.push(provider_root_item(provider, &root, b"/", name));
            }
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
        remote_entries.sort_by(|left, right| {
            left.display_name()
                .as_str()
                .cmp(right.display_name().as_str())
                .then_with(|| {
                    left.id()
                        .provider()
                        .as_str()
                        .cmp(right.id().provider().as_str())
                })
        });
        providers.insert(
            ProviderId::new(NETWORK_PROVIDER_ID).expect("the network provider ID is valid"),
            Arc::new(NetworkDiscoveryStore::with_entries(remote_entries)),
        );
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
    InvalidConnectionProfiles,
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
            Self::InvalidConnectionProfiles => {
                formatter.write_str("saved remote connections are invalid")
            }
        }
    }
}

impl std::error::Error for ProviderRuntimeError {}

struct StoreOnlyAdapter {
    store: Arc<dyn Store>,
    network_name: Option<Box<str>>,
}

impl StoreOnlyAdapter {
    fn new(store: Arc<dyn Store>) -> Self {
        Self {
            store,
            network_name: None,
        }
    }

    #[cfg(test)]
    fn network(store: Arc<dyn Store>, name: impl Into<Box<str>>) -> Self {
        Self {
            store,
            network_name: Some(name.into()),
        }
    }
}

impl ProviderAdapter for StoreOnlyAdapter {
    fn store(&self) -> Arc<dyn Store> {
        Arc::clone(&self.store)
    }

    fn network_name(&self) -> Option<&str> {
        self.network_name.as_deref()
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
    entries: Arc<[StoreItem]>,
}

impl NetworkDiscoveryStore {
    fn new() -> Self {
        let provider = ProviderId::new(NETWORK_PROVIDER_ID)
            .expect("the built-in network provider ID is valid");
        let root = StorePath::from_provider_key(provider.clone(), NETWORK_ROOT_KEY.to_vec())
            .expect("the built-in network root key is valid");
        Self {
            provider,
            root,
            entries: Arc::new([]),
        }
    }

    fn with_entries(entries: Vec<StoreItem>) -> Self {
        Self {
            entries: entries.into(),
            ..Self::new()
        }
    }

    fn root_item(&self) -> StoreItem {
        provider_root_item(&self.provider, &self.root, NETWORK_ROOT_KEY, "Network")
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
        let entries = Arc::clone(&self.entries);
        Box::pin(async move {
            cancellation.check()?;
            if !is_root {
                return Err(StoreError::Backend(
                    "network location does not exist".into(),
                ));
            }
            let start = request
                .continuation()
                .map_or(Ok(0), Continuation::decode_usize)?;
            if start > entries.len() {
                return Err(StoreError::InvalidContinuation);
            }
            let end = start.saturating_add(request.page_size()).min(entries.len());
            let next = (end < entries.len()).then(|| Continuation::from_usize(end));
            Page::try_new(
                &request,
                entries[start..end].to_vec(),
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
    use futures_lite::future;
    use musheen_core::ResourceLimits;
    use musheen_desktop::{
        ArchiveFormat, ArchiveLimits, ArchivePasswordProvider, ArchiveStore, ConnectionId,
        ConnectionProfile, ConnectionProfiles, OpendalStore, PasswordRequest, RemoteCasePolicy,
        RemoteHost, RemoteMutationPolicy, RemoteProtocol, SecurityPolicy, SettingsDocument,
    };
    use musheen_local::{DropAction, FileDragPayload};
    use std::fs::File;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn settings_with_remote_profile() -> SettingsDocument {
        let profile = ConnectionProfile::new(
            ConnectionId::new("team-files").unwrap(),
            "Team Files",
            RemoteProtocol::Ftp,
            RemoteHost::new(RemoteProtocol::Ftp, "files.example.test").unwrap(),
            None,
            "/",
            None::<&str>,
            None,
            SecurityPolicy::PlaintextConfirmed,
            None,
        )
        .unwrap();
        let mut settings = SettingsDocument::default();
        settings
            .set_value(
                "remote.connections",
                &ConnectionProfiles::new(vec![profile]).export().unwrap(),
            )
            .unwrap();
        settings
    }

    #[test]
    fn saved_profile_keeps_the_concrete_transfer_store_after_connect() {
        let settings = settings_with_remote_profile();
        let document = settings.value("remote.connections").unwrap();
        let profiles = ConnectionProfiles::import(&document).unwrap();
        let profile = profiles.profiles()[0].clone();
        let connector: RemoteStoreConnector = Arc::new(|provider, _, _| {
            Box::pin(async move {
                let operator =
                    opendal::Operator::new(opendal::services::Memory::default()).unwrap();
                let store = Arc::new(
                    OpendalStore::from_operator(
                        provider,
                        RemoteProtocol::Ftp,
                        operator,
                        RemoteCasePolicy::Sensitive,
                        RemoteMutationPolicy::CapabilitiesVerified,
                    )
                    .unwrap(),
                );
                Ok(remote::RemoteStoreConnection::opendal(store))
            })
        });
        let saved = RemoteProfileStore::new(profile, connector).unwrap();

        let transfer = future::block_on(saved.connect_transfer(CancellationToken::new())).unwrap();

        assert_eq!(transfer.provider_id(), saved.provider_id());
        assert_eq!(
            transfer.root_path(),
            StorePath::from_provider_key(saved.provider_id().clone(), b"/".to_vec()).unwrap()
        );
    }

    #[test]
    fn local_file_copy_uses_the_saved_remote_profile_route() {
        let settings = settings_with_remote_profile();
        let operator = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
        let connected_operator = operator.clone();
        let connector: RemoteStoreConnector = Arc::new(move |provider, _, _| {
            let operator = connected_operator.clone();
            Box::pin(async move {
                let store = Arc::new(
                    OpendalStore::from_operator(
                        provider,
                        RemoteProtocol::Ftp,
                        operator,
                        RemoteCasePolicy::Sensitive,
                        RemoteMutationPolicy::CapabilitiesVerified,
                    )
                    .unwrap(),
                );
                Ok(remote::RemoteStoreConnection::opendal(store))
            })
        });
        let runtime = ProviderRuntime::from_settings_with_connector(&settings, connector).unwrap();
        let network = runtime.store();
        let entries = future::block_on(network.read_directory(
            &network_root_path(),
            PageRequest::new(16, None).unwrap(),
            CancellationToken::new(),
        ))
        .unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let source_path = scratch.path().join("report.txt");
        std::fs::write(&source_path, b"remote copy payload").unwrap();
        let source = StorePath::from_unix_path(source_path);
        let target = entries.items()[0].path().clone();
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        runtime.configure_queue(&mut queue);

        let jobs = queue
            .submit_drop(
                FileDragPayload::new(vec![source], DropAction::Copy).unwrap(),
                target,
            )
            .unwrap();
        assert_eq!(jobs.len(), 1);
        let ready = queue.start_ready().unwrap();
        assert_eq!(ready.len(), 1);
        ready
            .into_iter()
            .next()
            .unwrap()
            .execute_detailed()
            .unwrap();

        assert_eq!(
            future::block_on(operator.read("report.txt"))
                .unwrap()
                .to_vec(),
            b"remote copy payload"
        );
        let listed = future::block_on(operator.list("/")).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path(), "report.txt");

        std::fs::write(scratch.path().join("report.txt"), b"replacement payload").unwrap();
        let mut retry = LocalOperationQueue::new(&ResourceLimits::default());
        runtime.configure_queue(&mut retry);
        retry
            .submit_drop(
                FileDragPayload::new(
                    vec![StorePath::from_unix_path(scratch.path().join("report.txt"))],
                    DropAction::Copy,
                )
                .unwrap(),
                entries.items()[0].path().clone(),
            )
            .unwrap();
        let failure = retry
            .start_ready()
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .execute_detailed()
            .unwrap_err();
        assert!(failure.message().contains("already exists"));
        assert_eq!(
            future::block_on(operator.read("report.txt"))
                .unwrap()
                .to_vec(),
            b"remote copy payload"
        );
    }

    #[test]
    fn saved_remote_profile_is_visible_without_opening_a_connection() {
        let settings = settings_with_remote_profile();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let connector: RemoteStoreConnector = Arc::new(move |_, _, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(StoreError::Backend("unexpected connection".into())) })
        });
        let runtime = ProviderRuntime::from_settings_with_connector(&settings, connector).unwrap();
        assert_eq!(runtime.routes.len(), 1);

        let network = runtime.store();
        let page = future::block_on(network.read_directory(
            &network_root_path(),
            PageRequest::new(16, None).unwrap(),
            CancellationToken::new(),
        ))
        .unwrap();
        assert_eq!(page.items().len(), 1);
        assert_eq!(page.items()[0].display_name().as_str(), "Team Files");
        let location = page.items()[0].path();
        assert!(
            location
                .provider_key()
                .unwrap()
                .0
                .as_str()
                .starts_with("musheen.remote.")
        );
        assert!(runtime.store().resolve_item(location).unwrap().is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn opening_saved_remote_profile_connects_once_for_repeated_reads() {
        let settings = settings_with_remote_profile();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let connector: RemoteStoreConnector = Arc::new(move |provider, _, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            let root = StorePath::from_provider_key(provider.clone(), b"/".to_vec()).unwrap();
            Box::pin(async move {
                Ok(remote::RemoteStoreConnection::browse_only(
                    Arc::new(NetworkDiscoveryStore {
                        provider,
                        root,
                        ..NetworkDiscoveryStore::new()
                    }) as Arc<dyn Store>,
                ))
            })
        });
        let runtime = ProviderRuntime::from_settings_with_connector(&settings, connector).unwrap();
        let network = runtime.store();
        let entries = future::block_on(network.read_directory(
            &network_root_path(),
            PageRequest::new(16, None).unwrap(),
            CancellationToken::new(),
        ))
        .unwrap();
        let remote = entries.items()[0].path();
        for _ in 0..2 {
            let page = future::block_on(network.read_directory(
                remote,
                PageRequest::new(16, None).unwrap(),
                CancellationToken::new(),
            ))
            .unwrap();
            assert!(page.items().is_empty());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn offline_profile_stays_visible_and_retries_after_a_failed_connection() {
        let settings = settings_with_remote_profile();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let connector: RemoteStoreConnector = Arc::new(move |provider, _, _| {
            let attempt = counted.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if attempt == 0 {
                    return Err(StoreError::Backend("remote service is offline".into()));
                }
                let root = StorePath::from_provider_key(provider.clone(), b"/".to_vec())
                    .expect("the fixture root is valid");
                Ok(remote::RemoteStoreConnection::browse_only(
                    Arc::new(NetworkDiscoveryStore {
                        provider,
                        root,
                        ..NetworkDiscoveryStore::new()
                    }) as Arc<dyn Store>,
                ))
            })
        });
        let runtime = ProviderRuntime::from_settings_with_connector(&settings, connector).unwrap();
        let store = runtime.store();
        let entries = future::block_on(store.read_directory(
            &network_root_path(),
            PageRequest::new(16, None).unwrap(),
            CancellationToken::new(),
        ))
        .unwrap();
        let remote = entries.items()[0].path();

        assert!(
            future::block_on(store.read_directory(
                remote,
                PageRequest::new(16, None).unwrap(),
                CancellationToken::new(),
            ))
            .is_err()
        );
        assert!(store.resolve_item(remote).unwrap().is_some());
        assert!(
            future::block_on(store.read_directory(
                remote,
                PageRequest::new(16, None).unwrap(),
                CancellationToken::new(),
            ))
            .is_ok()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn network_root_lists_registered_remote_locations() {
        let provider = ProviderId::new("musheen.remote.example").unwrap();
        let remote_root = StorePath::from_provider_key(provider.clone(), b"/".to_vec()).unwrap();
        let remote = NetworkDiscoveryStore {
            provider,
            root: remote_root.clone(),
            ..NetworkDiscoveryStore::new()
        };
        let runtime = ProviderRuntime::builder()
            .register_adapter(Arc::new(StoreOnlyAdapter::network(
                Arc::new(remote),
                "Example",
            )))
            .unwrap()
            .build()
            .unwrap();

        let page = future::block_on(runtime.store().read_directory(
            &network_root_path(),
            PageRequest::new(16, None).unwrap(),
            CancellationToken::new(),
        ))
        .unwrap();

        assert_eq!(page.items().len(), 1);
        assert_eq!(page.items()[0].path(), &remote_root);
    }

    #[test]
    fn equally_named_network_locations_have_stable_paged_order() {
        let mut builder = ProviderRuntime::builder();
        for suffix in b'a'..=b'j' {
            let provider = ProviderId::new(format!("musheen.remote.{}", suffix as char)).unwrap();
            let root = StorePath::from_provider_key(provider.clone(), b"/".to_vec()).unwrap();
            builder = builder
                .register_adapter(Arc::new(StoreOnlyAdapter::network(
                    Arc::new(NetworkDiscoveryStore {
                        provider,
                        root,
                        ..NetworkDiscoveryStore::new()
                    }),
                    "Shared",
                )))
                .unwrap();
        }
        let runtime = builder.build().unwrap();
        let store = runtime.store();
        let mut request = PageRequest::new(3, None).unwrap();
        let mut providers = Vec::new();
        loop {
            let page = future::block_on(store.read_directory(
                &network_root_path(),
                request,
                CancellationToken::new(),
            ))
            .unwrap();
            providers.extend(
                page.items()
                    .iter()
                    .map(|item| item.id().provider().as_str().to_owned()),
            );
            let Some(next) = page.next_request() else {
                break;
            };
            request = next;
        }
        assert_eq!(
            providers,
            (b'a'..=b'j')
                .map(|suffix| format!("musheen.remote.{}", suffix as char))
                .collect::<Vec<_>>()
        );
    }

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
