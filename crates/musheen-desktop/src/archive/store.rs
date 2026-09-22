use super::format::{RawArchiveEntry, RawEntryKind, read_entries};
use super::{ArchiveFormat, ArchivePath};
use musheen_core::{
    BoxFuture, CancellationToken, CapabilityKind, CapabilityMatrix, CapabilityReason,
    CapabilityState, Continuation, DirectoryWatch, DisplayPath, ItemId, ItemKind, MutationRequest,
    Page, PageRequest, ProviderId, Store, StoreError, StoreItem, StorePath, TotalHint,
};
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs::File;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArchiveError {
    UnsafePath(&'static str),
    DuplicatePath,
    LimitExceeded {
        resource: &'static str,
        value: usize,
        maximum: usize,
    },
    InvalidArchive,
    InvalidPassword,
    PasswordRequired,
    Io,
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsafePath(reason) => formatter.write_str(reason),
            Self::DuplicatePath => formatter.write_str("duplicate archive path"),
            Self::LimitExceeded {
                resource,
                value,
                maximum,
            } => write!(
                formatter,
                "{resource} limit exceeded: {value} is greater than {maximum}"
            ),
            Self::InvalidArchive => formatter.write_str("archive metadata is invalid"),
            Self::InvalidPassword => formatter.write_str("the archive password is invalid"),
            Self::PasswordRequired => formatter.write_str("the archive requires a password"),
            Self::Io => formatter.write_str("the archive could not be read"),
        }
    }
}

impl std::error::Error for ArchiveError {}

impl From<ArchiveError> for StoreError {
    fn from(error: ArchiveError) -> Self {
        Self::Backend(error.to_string().into_boxed_str())
    }
}

#[derive(Clone, Debug)]
pub struct ArchiveLimits {
    pub max_entries: usize,
    pub max_path_bytes: usize,
    pub max_metadata_bytes: usize,
    pub max_elapsed: Duration,
    pub max_nested_archives: usize,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_path_bytes: ArchivePath::MAX_BYTES,
            max_metadata_bytes: 512 * 1_024 * 1_024,
            max_elapsed: Duration::from_secs(10),
            max_nested_archives: 8,
        }
    }
}

pub struct ArchivePassword(Zeroizing<Vec<u8>>);

impl ArchivePassword {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl fmt::Debug for ArchivePassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ArchivePassword([REDACTED])")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PasswordRequest {
    pub format: ArchiveFormat,
}

pub trait ArchivePasswordProvider: Send + Sync {
    fn request_password(
        &self,
        request: &PasswordRequest,
    ) -> Result<Option<ArchivePassword>, ArchiveError>;
}

impl<F> ArchivePasswordProvider for F
where
    F: Fn(&PasswordRequest) -> Result<Option<ArchivePassword>, ArchiveError> + Send + Sync,
{
    fn request_password(
        &self,
        request: &PasswordRequest,
    ) -> Result<Option<ArchivePassword>, ArchiveError> {
        self(request)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ArchiveCounters {
    pub bytes_read: u64,
    pub metadata_bytes: usize,
    pub elapsed: Duration,
}

pub(crate) struct DecodeCounterState {
    bytes_read: AtomicU64,
    metadata_bytes: AtomicU64,
    elapsed: Mutex<Duration>,
}

impl DecodeCounterState {
    fn new() -> Self {
        Self {
            bytes_read: AtomicU64::new(0),
            metadata_bytes: AtomicU64::new(0),
            elapsed: Mutex::new(Duration::ZERO),
        }
    }

    pub(crate) fn add_read_bytes(&self, count: u64) {
        self.bytes_read.fetch_add(count, Ordering::Relaxed);
    }

    pub(crate) fn reserve_metadata(
        &self,
        count: usize,
        maximum: usize,
    ) -> Result<(), ArchiveError> {
        let count = u64::try_from(count).unwrap_or(u64::MAX);
        let maximum_u64 = u64::try_from(maximum).unwrap_or(u64::MAX);
        self.metadata_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current
                    .checked_add(count)
                    .filter(|next| *next <= maximum_u64)
            })
            .map(|_| ())
            .map_err(|current| ArchiveError::LimitExceeded {
                resource: "metadata bytes",
                value: usize::try_from(current.saturating_add(count)).unwrap_or(usize::MAX),
                maximum,
            })
    }

    fn finish(&self, elapsed: Duration) {
        *self
            .elapsed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = elapsed;
    }

    fn snapshot(&self) -> ArchiveCounters {
        ArchiveCounters {
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            metadata_bytes: usize::try_from(self.metadata_bytes.load(Ordering::Relaxed))
                .unwrap_or(usize::MAX),
            elapsed: *self
                .elapsed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        }
    }
}

pub struct ArchiveStore {
    provider: ProviderId,
    source: File,
    label: Box<str>,
    format: ArchiveFormat,
    passwords: Arc<dyn ArchivePasswordProvider>,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
    index: OnceLock<Result<ArchiveIndex, ArchiveError>>,
}

impl fmt::Debug for ArchiveStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArchiveStore")
            .field("provider", &self.provider)
            .field("label", &self.label)
            .field("format", &self.format)
            .field("limits", &self.limits)
            .field("counters", &self.counters())
            .finish_non_exhaustive()
    }
}

impl ArchiveStore {
    pub fn from_file(
        source: File,
        label: impl Into<Box<str>>,
        format: ArchiveFormat,
        passwords: Arc<dyn ArchivePasswordProvider>,
        limits: ArchiveLimits,
        nested_depth: usize,
    ) -> Result<Self, ArchiveError> {
        if nested_depth > limits.max_nested_archives {
            return Err(ArchiveError::LimitExceeded {
                resource: "nested archives",
                value: nested_depth,
                maximum: limits.max_nested_archives,
            });
        }
        if limits.max_entries == 0
            || limits.max_path_bytes == 0
            || limits.max_path_bytes > ArchivePath::MAX_BYTES
            || limits.max_metadata_bytes == 0
            || limits.max_elapsed.is_zero()
        {
            return Err(ArchiveError::LimitExceeded {
                resource: "archive limits",
                value: 0,
                maximum: 1,
            });
        }
        let label = label.into();
        let metadata = source.metadata().map_err(|_| ArchiveError::Io)?;
        if !metadata.is_file() {
            return Err(ArchiveError::InvalidArchive);
        }
        let mut identity = blake3::Hasher::new();
        identity.update(label.as_bytes());
        identity.update(&metadata.len().to_be_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            identity.update(&metadata.dev().to_be_bytes());
            identity.update(&metadata.ino().to_be_bytes());
            identity.update(&metadata.mtime().to_be_bytes());
            identity.update(&metadata.mtime_nsec().to_be_bytes());
        }
        let provider = ProviderId::new(format!("archive-{}", &identity.finalize().to_hex()[..24]))
            .expect("a hashed archive provider ID is valid");
        Ok(Self {
            provider,
            source,
            label,
            format,
            passwords,
            limits,
            counters: Arc::new(DecodeCounterState::new()),
            index: OnceLock::new(),
        })
    }

    #[must_use]
    pub fn root_path(&self) -> StorePath {
        StorePath::from_provider_key(self.provider.clone(), b".".to_vec().into_boxed_slice())
            .expect("the archive root sentinel is valid")
    }

    #[must_use]
    pub fn counters(&self) -> ArchiveCounters {
        self.counters.snapshot()
    }

    fn archive_path(&self, path: &StorePath) -> Result<ArchivePath, StoreError> {
        let Some((provider, key)) = path.provider_key() else {
            return Err(StoreError::Backend(
                "path does not belong to an archive".into(),
            ));
        };
        if provider != &self.provider {
            return Err(StoreError::Backend(
                "path belongs to another provider".into(),
            ));
        }
        if key == b"." {
            return Ok(ArchivePath::root());
        }
        ArchivePath::with_limit(key, self.limits.max_path_bytes).map_err(Into::into)
    }

    fn index(&self) -> Result<&ArchiveIndex, StoreError> {
        self.index
            .get_or_init(|| {
                let started = Instant::now();
                let parsed = read_entries(
                    &self.source,
                    self.format,
                    &self.limits,
                    &self.counters,
                    self.passwords.as_ref(),
                );
                let result = if started.elapsed() > self.limits.max_elapsed {
                    Err(elapsed_limit(started.elapsed(), self.limits.max_elapsed))
                } else {
                    parsed.and_then(|raw| {
                        ArchiveIndex::build(raw, &self.limits, &self.counters, started)
                    })
                };
                self.counters.finish(started.elapsed());
                result
            })
            .as_ref()
            .map_err(|error| error.clone().into())
    }

    fn store_item(&self, entry: &IndexedEntry) -> Result<StoreItem, StoreError> {
        let path = StorePath::from_provider_key(
            self.provider.clone(),
            entry.path.as_bytes().to_vec().into_boxed_slice(),
        )
        .map_err(|error| StoreError::Backend(error.to_string().into()))?;
        let id = ItemId::new(
            self.provider.clone(),
            entry.path.as_bytes().to_vec().into_boxed_slice(),
        )
        .map_err(|error| StoreError::Backend(error.to_string().into()))?;
        Ok(StoreItem::new(
            id,
            path,
            DisplayPath::new(String::from_utf8_lossy(entry.path.file_name()).into_owned()),
            entry.kind,
            entry.size,
        ))
    }
}

impl Store for ArchiveStore {
    fn provider_id(&self) -> &ProviderId {
        &self.provider
    }

    fn capabilities(&self, _location: &StorePath) -> CapabilityMatrix {
        CapabilityMatrix::new(|kind| {
            if kind == CapabilityKind::CaseSensitivity {
                return CapabilityState::Supported;
            }
            CapabilityState::Unsupported(
                CapabilityReason::new(match kind {
                    CapabilityKind::Watching => "archive contents do not provide live watching",
                    _ => "archive browsing is read-only",
                })
                .expect("the archive capability reason is valid"),
            )
        })
    }

    fn resolve_item(&self, path: &StorePath) -> Result<Option<StoreItem>, StoreError> {
        let path = self.archive_path(path)?;
        if path.is_root() {
            return Ok(None);
        }
        self.index()?
            .entries
            .get(&path)
            .map(|entry| self.store_item(entry))
            .transpose()
    }

    fn location_writable(&self, _path: &StorePath) -> Result<CapabilityState, StoreError> {
        Ok(CapabilityState::Unsupported(
            CapabilityReason::new("archive browsing is read-only")
                .expect("the archive writable reason is valid"),
        ))
    }

    fn executable_state(&self, _path: &StorePath) -> Result<CapabilityState, StoreError> {
        Ok(CapabilityState::Unsupported(
            CapabilityReason::new("archive entries are not executable in place")
                .expect("the archive executable reason is valid"),
        ))
    }

    fn read_directory<'a>(
        &'a self,
        location: &'a StorePath,
        request: PageRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Page<StoreItem>, StoreError>> {
        Box::pin(async move {
            cancellation.check()?;
            let location = self.archive_path(location)?;
            let index = self.index()?;
            if !location.is_root() {
                match index.entries.get(&location) {
                    Some(entry) if entry.kind == ItemKind::Directory => {}
                    Some(_) => {
                        return Err(StoreError::Backend(
                            "archive location is not a directory".into(),
                        ));
                    }
                    None => {
                        return Err(StoreError::Backend(
                            "archive directory no longer exists".into(),
                        ));
                    }
                }
            }
            let children = index
                .children
                .get(&location)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let start = decode_cursor(request.continuation(), &self.provider, &location)?;
            if start > children.len() {
                return Err(StoreError::InvalidContinuation);
            }
            let end = start
                .saturating_add(request.page_size())
                .min(children.len());
            let mut items = Vec::with_capacity(end - start);
            for path in &children[start..end] {
                cancellation.check()?;
                let entry = index
                    .entries
                    .get(path)
                    .expect("every child path has an indexed entry");
                items.push(self.store_item(entry)?);
            }
            let next =
                (end < children.len()).then(|| encode_cursor(&self.provider, &location, end));
            Page::try_new(
                &request,
                items,
                next,
                TotalHint::Exact(children.len() as u64),
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
                "watch directory",
                "archive contents do not provide live watching",
            ))
        })
    }

    fn validate_mutation(&self, request: &MutationRequest) -> Result<(), StoreError> {
        Err(request.unsupported("archive browsing is read-only"))
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

struct ArchiveIndex {
    entries: BTreeMap<ArchivePath, IndexedEntry>,
    children: BTreeMap<ArchivePath, Vec<ArchivePath>>,
}

#[derive(Clone)]
struct IndexedEntry {
    path: ArchivePath,
    kind: ItemKind,
    size: Option<u64>,
}

impl ArchiveIndex {
    fn build(
        raw_entries: Vec<RawArchiveEntry>,
        limits: &ArchiveLimits,
        counters: &DecodeCounterState,
        started: Instant,
    ) -> Result<Self, ArchiveError> {
        let mut entries = BTreeMap::<ArchivePath, IndexedEntry>::new();
        let mut explicit = HashSet::new();

        for raw in raw_entries {
            if started.elapsed() > limits.max_elapsed {
                return Err(elapsed_limit(started.elapsed(), limits.max_elapsed));
            }
            let path = ArchivePath::with_limit(&raw.path, limits.max_path_bytes)?;
            if path.is_root() || !explicit.insert(path.clone()) {
                return Err(ArchiveError::DuplicatePath);
            }
            for ancestor in path.ancestors() {
                if !entries.contains_key(&ancestor) {
                    reserve_entry(&ancestor, entries.len(), limits, counters)?;
                    entries.insert(
                        ancestor.clone(),
                        IndexedEntry {
                            path: ancestor,
                            kind: ItemKind::Directory,
                            size: None,
                        },
                    );
                }
            }
            let kind = match raw.kind {
                RawEntryKind::Directory => ItemKind::Directory,
                RawEntryKind::RegularFile => ItemKind::RegularFile,
                RawEntryKind::SymbolicLink => ItemKind::SymbolicLink,
                RawEntryKind::HardLink | RawEntryKind::Other => ItemKind::Other,
            };
            if let Some(existing) = entries.get_mut(&path) {
                if existing.kind != ItemKind::Directory || kind != ItemKind::Directory {
                    return Err(ArchiveError::DuplicatePath);
                }
                existing.size = raw.size;
            } else {
                reserve_entry(&path, entries.len(), limits, counters)?;
                entries.insert(
                    path.clone(),
                    IndexedEntry {
                        path,
                        kind,
                        size: raw.size,
                    },
                );
            }
        }

        let mut children = BTreeMap::<ArchivePath, Vec<ArchivePath>>::new();
        for path in entries.keys() {
            let parent = path.parent().expect("indexed entries are not roots");
            children.entry(parent).or_default().push(path.clone());
        }
        for paths in children.values_mut() {
            paths.sort_by(|left, right| left.file_name().cmp(right.file_name()));
        }
        Ok(Self { entries, children })
    }
}

fn reserve_entry(
    path: &ArchivePath,
    current_entries: usize,
    limits: &ArchiveLimits,
    counters: &DecodeCounterState,
) -> Result<(), ArchiveError> {
    if current_entries >= limits.max_entries {
        return Err(ArchiveError::LimitExceeded {
            resource: "archive entries",
            value: current_entries + 1,
            maximum: limits.max_entries,
        });
    }
    let bytes = path
        .as_bytes()
        .len()
        .checked_add(std::mem::size_of::<IndexedEntry>())
        .ok_or(ArchiveError::LimitExceeded {
            resource: "metadata bytes",
            value: usize::MAX,
            maximum: limits.max_metadata_bytes,
        })?;
    counters.reserve_metadata(bytes, limits.max_metadata_bytes)
}

fn elapsed_limit(elapsed: Duration, maximum: Duration) -> ArchiveError {
    ArchiveError::LimitExceeded {
        resource: "archive metadata milliseconds",
        value: usize::try_from(elapsed.as_millis()).unwrap_or(usize::MAX),
        maximum: usize::try_from(maximum.as_millis().max(1)).unwrap_or(usize::MAX),
    }
}

fn cursor_hash(provider: &ProviderId, location: &ArchivePath) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(provider.as_str().as_bytes());
    hasher.update(&[0]);
    hasher.update(location.as_bytes());
    hasher.finalize()
}

fn encode_cursor(provider: &ProviderId, location: &ArchivePath, offset: usize) -> Continuation {
    let hash = cursor_hash(provider, location);
    let mut bytes = Vec::with_capacity(24);
    bytes.extend_from_slice(&hash.as_bytes()[..16]);
    bytes.extend_from_slice(&(offset as u64).to_be_bytes());
    Continuation::new(bytes).expect("the fixed archive cursor is valid")
}

fn decode_cursor(
    continuation: Option<&Continuation>,
    provider: &ProviderId,
    location: &ArchivePath,
) -> Result<usize, StoreError> {
    let Some(continuation) = continuation else {
        return Ok(0);
    };
    let bytes = continuation.as_bytes();
    if bytes.len() != 24 || bytes[..16] != cursor_hash(provider, location).as_bytes()[..16] {
        return Err(StoreError::InvalidContinuation);
    }
    let encoded: [u8; 8] = bytes[16..]
        .try_into()
        .map_err(|_| StoreError::InvalidContinuation)?;
    usize::try_from(u64::from_be_bytes(encoded)).map_err(|_| StoreError::InvalidContinuation)
}
