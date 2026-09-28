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
use std::collections::VecDeque;
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
    /// Listings fetched through the session whose later pages are still to
    /// be served.
    listings: Mutex<SessionListings>,
}

/// How many session listings a window keeps for their later pages. Each is
/// dropped once its last page is served, so only listings still loading, or
/// left unfinished by a closed tab, count.
const OPEN_LISTINGS: usize = 8;

/// The session listings of one elevated window, oldest first. A listing's
/// continuation names its generation and offset, so all its pages come from
/// the snapshot its first page fetched, whatever other tabs list meanwhile.
#[derive(Default)]
struct SessionListings {
    next_generation: u64,
    open: VecDeque<(u64, PathBuf, Arc<[ListedEntry]>)>,
}

impl SessionListings {
    fn find(&self, generation: u64, folder: &Path) -> Option<Arc<[ListedEntry]>> {
        self.open
            .iter()
            .find(|(open, listed, _)| *open == generation && listed == folder)
            .map(|(_, _, entries)| Arc::clone(entries))
    }

    /// Keeps a listing whose later pages will be asked for.
    fn keep(&mut self, generation: u64, folder: &Path, entries: &Arc<[ListedEntry]>) {
        if self.open.iter().any(|(open, _, _)| *open == generation) {
            return;
        }
        if self.open.len() == OPEN_LISTINGS {
            self.open.pop_front();
        }
        self.open
            .push_back((generation, folder.to_path_buf(), Arc::clone(entries)));
    }

    fn drop_listing(&mut self, generation: u64) {
        self.open.retain(|(open, _, _)| *open != generation);
    }
}

/// A session listing's continuation: its generation, then the offset of the
/// next page, each as eight big-endian bytes.
fn session_continuation(generation: u64, offset: usize) -> musheen_core::Continuation {
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(&generation.to_be_bytes());
    bytes[8..].copy_from_slice(&(offset as u64).to_be_bytes());
    musheen_core::Continuation::new(bytes.to_vec())
        .expect("a 16-byte continuation fits the continuation limit")
}

fn decode_session_continuation(
    continuation: &musheen_core::Continuation,
) -> Result<(u64, usize), StoreError> {
    let bytes: [u8; 16] = continuation
        .as_bytes()
        .try_into()
        .map_err(|_| StoreError::InvalidContinuation)?;
    let (generation, offset) = bytes.split_at(8);
    let generation = u64::from_be_bytes(generation.try_into().expect("eight bytes"));
    let offset = u64::from_be_bytes(offset.try_into().expect("eight bytes"));
    Ok((
        generation,
        usize::try_from(offset).map_err(|_| StoreError::InvalidContinuation)?,
    ))
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
            listings: Mutex::new(SessionListings::default()),
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
            listings: Mutex::new(SessionListings::default()),
            session: Some(session),
        }
    }

    /// The listing a page of `relative` comes from, its generation, and the
    /// page's offset. A first page fetches a new listing through the
    /// session; a later page comes from the listing its continuation names,
    /// while the session is still open.
    async fn session_listing(
        &self,
        session: &Arc<dyn ElevatedSession>,
        relative: &Path,
        continuation: Option<&musheen_core::Continuation>,
        cancellation: CancellationToken,
    ) -> Result<(u64, Arc<[ListedEntry]>, usize), StoreError> {
        if let Some(continuation) = continuation {
            let (generation, offset) = decode_session_continuation(continuation)?;
            if !session.is_open() {
                return Err(Self::map_error(BrokerError::AuthorizationExpired));
            }
            let entries = self
                .listings
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .find(generation, relative)
                .ok_or_else(|| {
                    StoreError::Backend(
                        "the elevated listing is no longer held; reload the folder".into(),
                    )
                })?;
            return Ok((generation, entries, offset));
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
        let mut listings = self.listings.lock().unwrap_or_else(PoisonError::into_inner);
        let generation = listings.next_generation;
        listings.next_generation += 1;
        Ok((generation, entries, 0))
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
            // The English text of privilege-error-listing-too-large, so the
            // folder's error view shows it in the user's language.
            BrokerError::ListingTooLarge => {
                "The folder is too large to list as administrator: an elevated window lists at most 64 MiB"
            }
            // The English texts of privilege-error-answer-unreadable and
            // privilege-error-protocol-mismatch (SYS-034).
            BrokerError::AnswerUnreadable => {
                "The administrator session ended because Musheen could not read the broker's answer"
            }
            BrokerError::ProtocolMismatch => {
                "Musheen was updated while it was running. Restart Musheen to use administrator actions"
            }
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
            // A session listing names its generation in its continuation; a
            // listing of the local store pages by offset alone.
            let (generation, entries, offset) = if let Some(store) = &self.store {
                let entries: Arc<[ListedEntry]> = store
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
                    .collect();
                let offset = request
                    .continuation()
                    .map_or(Ok(0), musheen_core::Continuation::decode_usize)?;
                (None, entries, offset)
            } else {
                let session = self
                    .session
                    .as_ref()
                    .ok_or_else(|| StoreError::Backend("missing elevated broker".into()))?;
                let (generation, entries, offset) = self
                    .session_listing(
                        session,
                        &relative,
                        request.continuation(),
                        cancellation.clone(),
                    )
                    .await?;
                (Some(generation), entries, offset)
            };
            let total = entries.len();
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
            let more = next_offset < total;
            let next = match generation {
                None => more.then(|| musheen_core::Continuation::from_usize(next_offset)),
                Some(generation) => {
                    let mut listings = self.listings.lock().unwrap_or_else(PoisonError::into_inner);
                    if more {
                        listings.keep(generation, &relative, &entries);
                    } else {
                        listings.drop_listing(generation);
                    }
                    more.then(|| session_continuation(generation, next_offset))
                }
            };
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

    /// Whether the session still serves listings. A session whose broker
    /// ended, on its own or after a failure, is not open.
    fn is_open(&self) -> bool;

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

    fn is_open(&self) -> bool {
        BrokerSession::is_open(self)
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

    /// Reads the installed broker's protocol version, which Musheen does
    /// before a review asks for the sudo password (SYS-034).
    fn check_broker(
        &self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<(), BrokerError>> {
        let _ = cancellation;
        Box::pin(async { Ok(()) })
    }
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

    fn check_broker(
        &self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'_, Result<(), BrokerError>> {
        Box::pin(async move { self.transport.check_broker(&cancellation) })
    }
}
