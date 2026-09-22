use super::format::{ArchiveCopyContext, copy_entry, open_scanner};
use super::index::{ArchiveScannerFactory, IndexedEntry, LazyArchiveIndex};
use super::{ArchiveFormat, ArchivePath};
use musheen_core::{
    BoxFuture, CancellationToken, CapabilityKind, CapabilityMatrix, CapabilityReason,
    CapabilityState, Continuation, DirectoryWatch, DisplayPath, ItemId, MutationRequest, Page,
    PageRequest, ProviderId, Store, StoreError, StoreItem, StorePath, TotalHint,
};
use std::fmt;
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;
use zeroize::{Zeroize, Zeroizing};

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
    Cancelled,
    NotArchiveEntry,
    UnsupportedNestedFormat,
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
            Self::Cancelled => formatter.write_str("the archive operation was cancelled"),
            Self::NotArchiveEntry => formatter.write_str("the target is not an archive file entry"),
            Self::UnsupportedNestedFormat => {
                formatter.write_str("nested browsing is unavailable for this archive format")
            }
            Self::Io => formatter.write_str("the archive could not be read"),
        }
    }
}

impl std::error::Error for ArchiveError {}

impl From<ArchiveError> for StoreError {
    fn from(error: ArchiveError) -> Self {
        match error {
            ArchiveError::Cancelled => Self::Cancelled,
            other => Self::Backend(other.to_string().into_boxed_str()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ArchiveLimits {
    pub max_entries: usize,
    pub max_path_bytes: usize,
    pub max_metadata_bytes: usize,
    pub max_expanded_bytes: u64,
    pub max_compression_ratio: u64,
    pub max_elapsed: Duration,
    pub max_nested_archives: usize,
    pub max_nested_archive_bytes: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_path_bytes: ArchivePath::MAX_BYTES,
            max_metadata_bytes: 512 * 1_024 * 1_024,
            max_expanded_bytes: 20 * 1_024 * 1_024 * 1_024,
            max_compression_ratio: 1_000,
            max_elapsed: Duration::from_secs(10),
            max_nested_archives: 8,
            max_nested_archive_bytes: 512 * 1_024 * 1_024,
        }
    }
}

pub struct ArchivePassword {
    bytes: Zeroizing<Vec<u8>>,
    #[cfg(test)]
    drop_marker: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl ArchivePassword {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Zeroizing::new(bytes),
            #[cfg(test)]
            drop_marker: None,
        }
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }
}

impl Drop for ArchivePassword {
    fn drop(&mut self) {
        self.bytes.zeroize();
        #[cfg(test)]
        if let Some(marker) = self.drop_marker.take() {
            marker.store(true, Ordering::Release);
        }
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
    /// Current tracked heap bytes retained by codec and index structures.
    pub metadata_bytes: usize,
    pub peak_metadata_bytes: usize,
    pub total_allocated_bytes: u64,
    pub expanded_bytes: u64,
    pub elapsed: Duration,
}

pub(crate) struct DecodeCounterState {
    bytes_read: AtomicU64,
    metadata_bytes: AtomicU64,
    peak_metadata_bytes: AtomicU64,
    total_allocated_bytes: AtomicU64,
    expanded_bytes: AtomicU64,
    elapsed: Mutex<Duration>,
}

impl DecodeCounterState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            bytes_read: AtomicU64::new(0),
            metadata_bytes: AtomicU64::new(0),
            peak_metadata_bytes: AtomicU64::new(0),
            total_allocated_bytes: AtomicU64::new(0),
            expanded_bytes: AtomicU64::new(0),
            elapsed: Mutex::new(Duration::ZERO),
        })
    }

    pub(crate) fn add_read_bytes(&self, count: u64) {
        self.bytes_read.fetch_add(count, Ordering::Relaxed);
    }

    pub(crate) fn add_elapsed(&self, elapsed: Duration) {
        let mut total = self
            .elapsed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *total = total.saturating_add(elapsed);
    }

    pub(crate) fn elapsed(&self) -> Duration {
        *self
            .elapsed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn reserve(
        self: &Arc<Self>,
        count: usize,
        maximum: usize,
    ) -> Result<AllocationLease, ArchiveError> {
        let count = u64::try_from(count).unwrap_or(u64::MAX);
        let maximum_u64 = u64::try_from(maximum).unwrap_or(u64::MAX);
        let previous = self
            .metadata_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(count)
                    .filter(|next| *next <= maximum_u64)
            })
            .map_err(|current| ArchiveError::LimitExceeded {
                resource: "metadata bytes",
                value: usize::try_from(current.saturating_add(count)).unwrap_or(usize::MAX),
                maximum,
            })?;
        let current = previous.saturating_add(count);
        self.peak_metadata_bytes
            .fetch_max(current, Ordering::Relaxed);
        self.total_allocated_bytes
            .fetch_add(count, Ordering::Relaxed);
        Ok(AllocationLease {
            counters: Some(Arc::clone(self)),
            bytes: count,
        })
    }

    pub(crate) fn try_reserve_external(self: &Arc<Self>, count: usize, maximum: usize) -> bool {
        match self.reserve(count, maximum) {
            Ok(mut lease) => {
                lease.counters.take();
                true
            }
            Err(_) => false,
        }
    }

    pub(crate) fn release_external(&self, count: usize) {
        self.metadata_bytes
            .fetch_sub(u64::try_from(count).unwrap_or(u64::MAX), Ordering::AcqRel);
    }

    pub(crate) fn charge_expanded(
        &self,
        count: u64,
        source_bytes: u64,
        limits: &ArchiveLimits,
    ) -> Result<(), ArchiveError> {
        let ratio_limit = source_bytes.saturating_mul(limits.max_compression_ratio);
        let effective_limit = limits.max_expanded_bytes.min(ratio_limit);
        self.expanded_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(count)
                    .filter(|next| *next <= effective_limit)
            })
            .map(|_| ())
            .map_err(|current| {
                let value = current.saturating_add(count);
                if value > limits.max_expanded_bytes {
                    ArchiveError::LimitExceeded {
                        resource: "expanded bytes",
                        value: usize::try_from(value).unwrap_or(usize::MAX),
                        maximum: usize::try_from(limits.max_expanded_bytes).unwrap_or(usize::MAX),
                    }
                } else {
                    ArchiveError::LimitExceeded {
                        resource: "compression ratio",
                        value: usize::try_from(value).unwrap_or(usize::MAX),
                        maximum: usize::try_from(ratio_limit).unwrap_or(usize::MAX),
                    }
                }
            })
    }

    fn snapshot(&self) -> ArchiveCounters {
        ArchiveCounters {
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            metadata_bytes: usize::try_from(self.metadata_bytes.load(Ordering::Relaxed))
                .unwrap_or(usize::MAX),
            peak_metadata_bytes: usize::try_from(self.peak_metadata_bytes.load(Ordering::Relaxed))
                .unwrap_or(usize::MAX),
            total_allocated_bytes: self.total_allocated_bytes.load(Ordering::Relaxed),
            expanded_bytes: self.expanded_bytes.load(Ordering::Relaxed),
            elapsed: self.elapsed(),
        }
    }
}

pub(crate) struct AllocationLease {
    counters: Option<Arc<DecodeCounterState>>,
    bytes: u64,
}

impl fmt::Debug for AllocationLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AllocationLease")
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl Drop for AllocationLease {
    fn drop(&mut self) {
        if let Some(counters) = self.counters.take() {
            counters
                .metadata_bytes
                .fetch_sub(self.bytes, Ordering::AcqRel);
        }
    }
}

#[derive(Clone, Debug)]
struct ArchiveLineage {
    identity: [u8; 16],
    depth: usize,
}

pub struct ArchiveStore {
    provider: ProviderId,
    source: File,
    _temporary_source: Option<NamedTempFile>,
    format: ArchiveFormat,
    passwords: Arc<dyn ArchivePasswordProvider>,
    limits: ArchiveLimits,
    lineage: ArchiveLineage,
    counters: Arc<DecodeCounterState>,
    index: OnceLock<Result<Mutex<LazyArchiveIndex>, ArchiveError>>,
}

impl fmt::Debug for ArchiveStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArchiveStore")
            .field("provider", &self.provider)
            .field("format", &self.format)
            .field("nesting_depth", &self.lineage.depth)
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
    ) -> Result<Self, ArchiveError> {
        let label = label.into();
        let metadata = validate_source(&source, &limits)?;
        let mut root_hash = blake3::Hasher::new();
        hash_source_identity(&mut root_hash, &label, &metadata);
        let mut identity = [0_u8; 16];
        identity.copy_from_slice(&root_hash.finalize().as_bytes()[..16]);
        Self::with_lineage(
            source,
            None,
            format,
            passwords,
            limits,
            ArchiveLineage { identity, depth: 0 },
        )
    }

    fn with_lineage(
        source: File,
        temporary_source: Option<NamedTempFile>,
        format: ArchiveFormat,
        passwords: Arc<dyn ArchivePasswordProvider>,
        limits: ArchiveLimits,
        lineage: ArchiveLineage,
    ) -> Result<Self, ArchiveError> {
        validate_source(&source, &limits)?;
        if lineage.depth > limits.max_nested_archives {
            return Err(ArchiveError::LimitExceeded {
                resource: "nested archives",
                value: lineage.depth,
                maximum: limits.max_nested_archives,
            });
        }
        let provider = ProviderId::new(format!(
            "archive-{}",
            &blake3::hash(&lineage.identity).to_hex()[..24]
        ))
        .expect("a hashed archive provider ID is valid");
        Ok(Self {
            provider,
            source,
            _temporary_source: temporary_source,
            format,
            passwords,
            limits,
            lineage,
            counters: DecodeCounterState::new(),
            index: OnceLock::new(),
        })
    }

    #[must_use]
    pub fn nesting_depth(&self) -> usize {
        self.lineage.depth
    }

    #[must_use]
    pub fn root_path(&self) -> StorePath {
        ArchivePath::root(self.provider.clone())
            .to_store_path()
            .expect("the archive root path is valid")
    }

    #[must_use]
    pub fn counters(&self) -> ArchiveCounters {
        self.counters.snapshot()
    }

    pub fn open_nested(
        &self,
        path: &StorePath,
        format: ArchiveFormat,
        cancellation: CancellationToken,
    ) -> Result<Self, ArchiveError> {
        cancellation.check().map_err(|_| ArchiveError::Cancelled)?;
        let path = self.archive_path(path).map_err(store_error_to_archive)?;
        let (entry, entry_size, entry_compressed_size, entry_kind) = {
            let index = self.index().map_err(store_error_to_archive)?;
            let mut index = index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entry = index
                .find_entry(&path, &cancellation, &self.limits, &self.counters)?
                .ok_or(ArchiveError::NotArchiveEntry)?;
            (
                entry.locator.ok_or(ArchiveError::NotArchiveEntry)?,
                entry.size,
                entry.compressed_size,
                entry.kind,
            )
        };
        let child_depth = self.lineage.depth.saturating_add(1);
        if child_depth > self.limits.max_nested_archives {
            return Err(ArchiveError::LimitExceeded {
                resource: "nested archives",
                value: child_depth,
                maximum: self.limits.max_nested_archives,
            });
        }
        let mut temporary = NamedTempFile::new().map_err(|_| ArchiveError::Io)?;
        let started = Instant::now();
        let copy_result = copy_entry(
            &self.source,
            self.format,
            entry,
            &mut temporary,
            ArchiveCopyContext {
                passwords: self.passwords.as_ref(),
                limits: &self.limits,
                counters: &self.counters,
                cancellation: &cancellation,
                compressed_size: entry_compressed_size,
            },
        );
        self.counters.add_elapsed(started.elapsed());
        if self.counters.elapsed() > self.limits.max_elapsed {
            return Err(elapsed_limit(
                self.counters.elapsed(),
                self.limits.max_elapsed,
            ));
        }
        copy_result?;
        temporary
            .as_file_mut()
            .seek(SeekFrom::Start(0))
            .map_err(|_| ArchiveError::Io)?;
        let source = temporary.reopen().map_err(|_| ArchiveError::Io)?;
        let mut child_identity = blake3::Hasher::new();
        child_identity.update(self.provider.as_str().as_bytes());
        child_identity.update(path.as_bytes());
        child_identity.update(&entry.to_be_bytes());
        child_identity.update(&entry_size.unwrap_or(u64::MAX).to_be_bytes());
        child_identity.update(&entry_compressed_size.unwrap_or(u64::MAX).to_be_bytes());
        child_identity.update(&[entry_kind as u8, archive_format_tag(format)]);
        let mut identity = [0_u8; 16];
        identity.copy_from_slice(&child_identity.finalize().as_bytes()[..16]);
        Self::with_lineage(
            source,
            Some(temporary),
            format,
            Arc::clone(&self.passwords),
            self.limits.clone(),
            ArchiveLineage {
                identity,
                depth: child_depth,
            },
        )
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
            return Ok(ArchivePath::root(self.provider.clone()));
        }
        ArchivePath::with_limit(self.provider.clone(), key, self.limits.max_path_bytes)
            .map_err(Into::into)
    }

    fn index(&self) -> Result<&Mutex<LazyArchiveIndex>, StoreError> {
        self.index
            .get_or_init(|| {
                let started = Instant::now();
                let source = self.source.try_clone().map_err(|_| ArchiveError::Io)?;
                let provider = self.provider.clone();
                let format = self.format;
                let limits = self.limits.clone();
                let counters = Arc::clone(&self.counters);
                let passwords = Arc::clone(&self.passwords);
                let scanner_factory: ArchiveScannerFactory = Arc::new(move || {
                    open_scanner(
                        &source,
                        provider.clone(),
                        format,
                        &limits,
                        &counters,
                        passwords.as_ref(),
                    )
                });
                let scanner = scanner_factory();
                self.counters.add_elapsed(started.elapsed());
                if self.counters.elapsed() > self.limits.max_elapsed {
                    return Err(elapsed_limit(
                        self.counters.elapsed(),
                        self.limits.max_elapsed,
                    ));
                }
                let source_bytes = self.source.metadata().map_err(|_| ArchiveError::Io)?.len();
                scanner.map(|scanner| {
                    Mutex::new(LazyArchiveIndex::new(
                        scanner,
                        scanner_factory,
                        source_bytes,
                    ))
                })
            })
            .as_ref()
            .map_err(|error| error.clone().into())
    }

    fn store_item(
        &self,
        path: &ArchivePath,
        entry: &IndexedEntry,
    ) -> Result<StoreItem, StoreError> {
        let store_path = path.to_store_path().map_err(StoreError::from)?;
        let id = ItemId::new(
            self.provider.clone(),
            path.as_bytes().to_vec().into_boxed_slice(),
        )
        .map_err(|error| StoreError::Backend(error.to_string().into()))?;
        Ok(StoreItem::new(
            id,
            store_path,
            DisplayPath::new(String::from_utf8_lossy(path.file_name()).into_owned()),
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
        let index = self.index()?;
        let mut index = index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        index
            .find_entry(
                &path,
                &CancellationToken::new(),
                &self.limits,
                &self.counters,
            )
            .map_err(StoreError::from)?
            .map(|entry| self.store_item(&path, entry))
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
            let start = decode_cursor(request.continuation(), &self.provider, &location)?;
            let required = start.saturating_add(request.page_size());
            let index = self.index()?;
            let mut index = index
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            index
                .ensure_directory(&location, &cancellation, &self.limits, &self.counters)
                .map_err(StoreError::from)?;
            index
                .ensure_children(
                    &location,
                    required,
                    &cancellation,
                    &self.limits,
                    &self.counters,
                )
                .map_err(StoreError::from)?;
            let children = index
                .children
                .get(&location)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            if start > children.len() {
                return Err(StoreError::InvalidContinuation);
            }
            let end = required.min(children.len());
            let mut items = Vec::with_capacity(end - start);
            for path in &children[start..end] {
                cancellation.check()?;
                let entry = index
                    .entries
                    .get(path)
                    .expect("every child path has an indexed entry");
                items.push(self.store_item(path, entry)?);
            }
            let next = (!index.finished || end < children.len())
                .then(|| encode_cursor(&self.provider, &location, end));
            let total_hint = if index.finished {
                TotalHint::Exact(children.len() as u64)
            } else {
                TotalHint::AtLeast(children.len() as u64)
            };
            Page::try_new(&request, items, next, total_hint)
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

fn validate_source(
    source: &File,
    limits: &ArchiveLimits,
) -> Result<std::fs::Metadata, ArchiveError> {
    if limits.max_entries == 0
        || limits.max_path_bytes == 0
        || limits.max_path_bytes > ArchivePath::MAX_BYTES
        || limits.max_metadata_bytes == 0
        || limits.max_expanded_bytes == 0
        || limits.max_compression_ratio == 0
        || limits.max_elapsed.is_zero()
        || limits.max_nested_archive_bytes == 0
    {
        return Err(ArchiveError::LimitExceeded {
            resource: "archive limits",
            value: 0,
            maximum: 1,
        });
    }
    let metadata = source.metadata().map_err(|_| ArchiveError::Io)?;
    if !metadata.is_file() {
        return Err(ArchiveError::InvalidArchive);
    }
    Ok(metadata)
}

fn archive_format_tag(format: ArchiveFormat) -> u8 {
    match format {
        ArchiveFormat::Zip => 1,
        ArchiveFormat::Tar => 2,
        ArchiveFormat::TarGzip => 3,
        ArchiveFormat::TarZstd => 4,
        ArchiveFormat::SevenZip => 5,
        #[cfg(feature = "archive-libarchive")]
        ArchiveFormat::Rar => 6,
        #[cfg(feature = "archive-libarchive")]
        ArchiveFormat::Iso => 7,
    }
}

fn hash_source_identity(identity: &mut blake3::Hasher, label: &str, metadata: &std::fs::Metadata) {
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
}

pub(crate) fn elapsed_limit(elapsed: Duration, maximum: Duration) -> ArchiveError {
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

fn store_error_to_archive(error: StoreError) -> ArchiveError {
    match error {
        StoreError::Cancelled => ArchiveError::Cancelled,
        _ => ArchiveError::InvalidArchive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn archive_password_is_redacted_and_runs_secret_teardown() {
        let marker = Arc::new(AtomicBool::new(false));
        let password = ArchivePassword {
            bytes: Zeroizing::new(b"do not log me".to_vec()),
            drop_marker: Some(Arc::clone(&marker)),
        };
        assert_eq!(format!("{password:?}"), "ArchivePassword([REDACTED])");
        drop(password);
        assert!(marker.load(Ordering::Acquire));
    }
}
