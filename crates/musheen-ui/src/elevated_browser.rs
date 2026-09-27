use crate::{AppearanceMode, Catalog};
use musheen_core::{
    BoxFuture, CancellationToken, CapabilityMatrix, CapabilityReason, CapabilityState,
    DirectoryWatch, DisplayPath, ItemId, ItemKind, MutationRequest, Page, PageRequest, ProviderId,
    Store, StoreError, StoreItem, StorePath, TotalHint,
};
use musheen_desktop::{
    BrokerDirectoryEntry, BrokerError, BrokerLaunch, BrokerOutput, BrokerRequest, BrokerSession,
    BrokerTransport, Clock, ElevatedRootReference, INSTALLED_BROKER_PATH, PrivilegeProvider,
    ProcessBrokerTransport, RootedEntryKind, RootedStore, SecretBuffer, SudoPtyBrokerTransport,
};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// One entry of a listing: name, identity, kind, size and modification time.
type ListedEntry = (
    OsString,
    [u8; 16],
    RootedEntryKind,
    Option<u64>,
    Option<i64>,
);

pub struct ElevatedBrowser<C> {
    store: Arc<RootedStore<C>>,
    catalog: Catalog,
    relative_location: PathBuf,
}

impl<C: Clock> ElevatedBrowser<C> {
    #[must_use]
    pub fn new(store: RootedStore<C>, catalog: Catalog) -> Self {
        Self {
            store: Arc::new(store),
            catalog,
            relative_location: PathBuf::new(),
        }
    }

    #[must_use]
    pub fn chrome(&self, appearance: AppearanceMode) -> ElevatedChrome<'_> {
        ElevatedChrome {
            warning: self
                .catalog
                .message("elevated-browser-warning")
                .expect("the elevated warning is localized"),
            appearance,
        }
    }

    #[must_use]
    pub fn relative_location(&self) -> &Path {
        &self.relative_location
    }

    pub fn navigate(&mut self, relative: &Path) -> Result<(), BrokerError> {
        self.store.resolve(relative)?;
        self.relative_location = relative.to_path_buf();
        Ok(())
    }

    pub fn navigate_breadcrumb(&mut self, index: usize) -> Result<(), BrokerError> {
        let components = self
            .relative_location
            .components()
            .take(index.saturating_add(1))
            .collect::<PathBuf>();
        self.navigate(&components)
    }

    #[must_use]
    pub fn store(&self) -> &RootedStore<C> {
        self.store.as_ref()
    }
}

pub struct RootedFilesystemStore<C> {
    provider: ProviderId,
    root: PathBuf,
    root_identity: Box<[u8]>,
    store: Option<Arc<RootedStore<C>>>,
    session: Option<Arc<dyn ElevatedSession>>,
    /// The last listing fetched through the session, which serves that
    /// folder's later pages without asking the broker again.
    listing: Mutex<Option<(PathBuf, Arc<[ListedEntry]>)>>,
}

impl<C: Clock> RootedFilesystemStore<C> {
    #[must_use]
    pub fn new(store: RootedStore<C>) -> Self {
        let root = store.grant().root().to_path_buf();
        let root_identity = store.grant().grant_id().as_bytes().into();
        Self {
            provider: ProviderId::new("local").expect("the built-in provider ID is valid"),
            root,
            root_identity,
            store: Some(Arc::new(store)),
            session: None,
            listing: Mutex::new(None),
        }
    }

    /// The store of an elevated window, whose listings go through the
    /// window's broker session. Dropping the store ends the session.
    #[must_use]
    pub fn remote(session: Arc<dyn ElevatedSession>) -> Self {
        let root_reference = session.root_reference();
        Self {
            provider: ProviderId::new("local").expect("the built-in provider ID is valid"),
            root: root_reference.root().to_path_buf(),
            root_identity: root_reference.identity().to_vec().into_boxed_slice(),
            store: None,
            listing: Mutex::new(None),
            session: Some(session),
        }
    }

    /// Lists `relative` through the session. A continuation page of the
    /// folder listed last comes from that listing.
    async fn session_listing(
        &self,
        session: &Arc<dyn ElevatedSession>,
        relative: &Path,
        continued: bool,
        cancellation: CancellationToken,
    ) -> Result<Arc<[ListedEntry]>, StoreError> {
        if continued
            && let Some((listed, entries)) = self
                .listing
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
            && listed == relative
        {
            return Ok(Arc::clone(entries));
        }
        let entries = session
            .read_directory(
                session.root_reference().clone(),
                relative.to_path_buf(),
                cancellation,
            )
            .await
            .map_err(Self::map_error)?
            .into_iter()
            .map(|entry| {
                (
                    OsString::from_vec(entry.name().to_vec()),
                    *entry.identity(),
                    entry.kind(),
                    entry.size(),
                    entry.modified_unix_seconds(),
                )
            })
            .collect::<Arc<[_]>>();
        *self.listing.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((relative.to_path_buf(), Arc::clone(&entries)));
        Ok(entries)
    }

    #[must_use]
    pub fn rooted_store(&self) -> Option<&RootedStore<C>> {
        self.store.as_deref()
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn relative(&self, location: &StorePath) -> Result<PathBuf, StoreError> {
        let path = location
            .as_unix_path()
            .ok_or_else(|| StoreError::Backend("elevated browsing is local-only".into()))?;
        path.strip_prefix(&self.root)
            .map(Path::to_path_buf)
            .map_err(|_| StoreError::Backend("location is outside the elevated root".into()))
    }

    fn map_error(error: BrokerError) -> StoreError {
        let reason = match error {
            BrokerError::AuthorizationExpired => "the elevated authorization expired",
            BrokerError::ScopeEscape => "the location leaves the elevated root",
            BrokerError::SymlinkRefused => "a symbolic link requires new authorization",
            BrokerError::TargetReplaced => "the elevated root was replaced",
            _ => "the elevated broker could not read this location",
        };
        StoreError::Backend(reason.into())
    }
}

impl<C: Clock> Store for RootedFilesystemStore<C> {
    fn provider_id(&self) -> &ProviderId {
        &self.provider
    }

    fn capabilities(&self, _location: &StorePath) -> CapabilityMatrix {
        CapabilityMatrix::new(|_| {
            CapabilityState::Unsupported(
                CapabilityReason::new(
                    "elevated mutations require a separately authorized broker operation",
                )
                .expect("the elevated capability reason is valid"),
            )
        })
    }

    fn resolve_item(&self, path: &StorePath) -> Result<Option<StoreItem>, StoreError> {
        let relative = self.relative(path)?;
        if let Some(store) = &self.store {
            store.resolve(&relative).map_err(Self::map_error)?;
        }
        let key = if relative.as_os_str().is_empty() {
            self.root_identity.to_vec()
        } else {
            relative.as_os_str().as_bytes().to_vec()
        };
        Ok(Some(StoreItem::new(
            ItemId::new(self.provider.clone(), key)
                .map_err(|error| StoreError::Backend(error.to_string().into()))?,
            path.clone(),
            DisplayPath::new(path.as_unix_path().and_then(Path::file_name).map_or_else(
                || self.root.to_string_lossy().into_owned(),
                |name| name.to_string_lossy().into_owned(),
            )),
            ItemKind::Directory,
            None,
        )))
    }

    fn read_directory<'a>(
        &'a self,
        location: &'a StorePath,
        request: PageRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Page<StoreItem>, StoreError>> {
        Box::pin(async move {
            cancellation.check()?;
            let relative = self.relative(location)?;
            let entries: Arc<[ListedEntry]> = if let Some(store) = &self.store {
                store
                    .read_directory(&relative)
                    .map_err(Self::map_error)?
                    .into_iter()
                    .map(|entry| {
                        (
                            entry.name().to_os_string(),
                            *entry.identity(),
                            entry.kind(),
                            entry.size(),
                            entry.modified_unix_seconds(),
                        )
                    })
                    .collect()
            } else {
                let session = self
                    .session
                    .as_ref()
                    .ok_or_else(|| StoreError::Backend("missing elevated broker".into()))?;
                self.session_listing(
                    session,
                    &relative,
                    request.continuation().is_some(),
                    cancellation.clone(),
                )
                .await?
            };
            let total = entries.len();
            let offset = request
                .continuation()
                .map_or(Ok(0), musheen_core::Continuation::decode_usize)?;
            if offset > total {
                return Err(StoreError::InvalidContinuation);
            }
            let root = &self.root;
            let items = entries
                .iter()
                .skip(offset)
                .take(request.page_size())
                .map(|&(ref name, identity, entry_kind, size, modified)| {
                    let path = root.join(&relative).join(name);
                    let kind = match entry_kind {
                        RootedEntryKind::Directory => ItemKind::Directory,
                        RootedEntryKind::RegularFile => ItemKind::RegularFile,
                        RootedEntryKind::SymbolicLink => ItemKind::SymbolicLink,
                        RootedEntryKind::Other => ItemKind::Other,
                    };
                    let item = StoreItem::new(
                        ItemId::new(self.provider.clone(), identity.to_vec())
                            .expect("filesystem identities fit the item key limit"),
                        StorePath::from_unix_path(path.into_os_string()),
                        DisplayPath::new(name.to_string_lossy().into_owned()),
                        kind,
                        size,
                    );
                    if let Some(modified) = modified {
                        item.with_modified_unix_seconds(modified)
                    } else {
                        item
                    }
                })
                .collect::<Vec<_>>();
            let next_offset = offset.saturating_add(items.len());
            let next =
                (next_offset < total).then(|| musheen_core::Continuation::from_usize(next_offset));
            Page::try_new(&request, items, next, TotalHint::Exact(total as u64))
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
                "elevated roots do not expose an unprivileged filesystem watcher",
            ))
        })
    }

    fn validate_mutation(&self, request: &MutationRequest) -> Result<(), StoreError> {
        Err(request.unsupported("elevated mutations require a fresh broker authorization"))
    }

    fn mutate<'a>(
        &'a self,
        request: MutationRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            cancellation.check()?;
            Err(request.unsupported("elevated mutations require a fresh broker authorization"))
        })
    }
}

pub struct ElevatedChrome<'a> {
    warning: &'a str,
    appearance: AppearanceMode,
}

impl ElevatedChrome<'_> {
    #[must_use]
    pub const fn warning(&self) -> &str {
        self.warning
    }

    #[must_use]
    pub const fn icon(&self) -> &'static str {
        "shield"
    }

    #[must_use]
    pub const fn always_visible(&self) -> bool {
        true
    }

    #[must_use]
    pub const fn distinct_from_ordinary_chrome(&self) -> bool {
        match self.appearance {
            AppearanceMode::Light | AppearanceMode::Dark | AppearanceMode::HighContrast => true,
        }
    }

    #[must_use]
    pub fn accessible_name(&self) -> String {
        format!("{}: {}", self.icon(), self.warning)
    }
}

/// The broker session of one elevated window (SYS-034): authorized once by
/// Open as Administrator, it lists folders under the granted root until the
/// window drops it.
pub trait ElevatedSession: Send + Sync + 'static {
    fn root_reference(&self) -> &ElevatedRootReference;

    fn read_directory<'a>(
        &'a self,
        root: ElevatedRootReference,
        relative: PathBuf,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<BrokerDirectoryEntry>, BrokerError>>;
}

impl ElevatedSession for BrokerSession {
    fn root_reference(&self) -> &ElevatedRootReference {
        BrokerSession::root_reference(self)
    }

    fn read_directory<'a>(
        &'a self,
        root: ElevatedRootReference,
        relative: PathBuf,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Vec<BrokerDirectoryEntry>, BrokerError>> {
        Box::pin(async move { BrokerSession::read_directory(self, root, &relative, &cancellation) })
    }
}

pub trait PrivilegeBackend: Send + Sync + 'static {
    fn provider(&self) -> PrivilegeProvider;

    /// Runs one request under its own authorization, as Run as
    /// Administrator does.
    fn perform<'a>(
        &'a self,
        request: &'a BrokerRequest,
        cancellation: CancellationToken,
        authentication: Option<SecretBuffer>,
    ) -> BoxFuture<'a, Result<BrokerOutput, BrokerError>>;

    /// Authorizes an Open as Administrator request once and returns the
    /// session that serves the elevated window's listings.
    fn open_window<'a>(
        &'a self,
        request: &'a BrokerRequest,
        cancellation: CancellationToken,
        authentication: Option<SecretBuffer>,
    ) -> BoxFuture<'a, Result<Arc<dyn ElevatedSession>, BrokerError>>;
}

pub struct SystemPrivilegeBackend {
    provider: PrivilegeProvider,
    transport: Arc<dyn BrokerTransport>,
}

impl SystemPrivilegeBackend {
    #[must_use]
    pub fn new(provider: PrivilegeProvider) -> Self {
        let launch = BrokerLaunch::new(INSTALLED_BROKER_PATH, provider);
        let transport: Arc<dyn BrokerTransport> = match provider {
            PrivilegeProvider::Polkit => Arc::new(ProcessBrokerTransport::new(launch)),
            PrivilegeProvider::Sudo => Arc::new(SudoPtyBrokerTransport::new(launch)),
        };
        Self {
            provider,
            transport,
        }
    }

    #[must_use]
    pub fn with_transport(
        provider: PrivilegeProvider,
        transport: Arc<dyn BrokerTransport>,
    ) -> Self {
        Self {
            provider,
            transport,
        }
    }
}

impl PrivilegeBackend for SystemPrivilegeBackend {
    fn provider(&self) -> PrivilegeProvider {
        self.provider
    }

    fn perform<'a>(
        &'a self,
        request: &'a BrokerRequest,
        cancellation: CancellationToken,
        authentication: Option<SecretBuffer>,
    ) -> BoxFuture<'a, Result<BrokerOutput, BrokerError>> {
        Box::pin(async move {
            self.transport
                .perform_with_authentication(request, &cancellation, authentication)
        })
    }

    fn open_window<'a>(
        &'a self,
        request: &'a BrokerRequest,
        cancellation: CancellationToken,
        authentication: Option<SecretBuffer>,
    ) -> BoxFuture<'a, Result<Arc<dyn ElevatedSession>, BrokerError>> {
        Box::pin(async move {
            let session = self
                .transport
                .open_session(request, &cancellation, authentication)?;
            Ok(Arc::new(session) as Arc<dyn ElevatedSession>)
        })
    }
}
