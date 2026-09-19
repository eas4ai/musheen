use crate::{EventGeneration, JobId, MetadataKind, MetadataReport, StagingPath, source_unchanged};
use musheen_core::{CancellationToken, StoreError, StorePath};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    RegularFile,
    Directory,
    SymbolicLink,
    BlockDevice,
    CharacterDevice,
    Fifo,
    Socket,
}

impl EntryKind {
    const fn copyable(self) -> bool {
        matches!(
            self,
            Self::RegularFile | Self::Directory | Self::SymbolicLink
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntrySnapshot {
    identity: Box<[u8]>,
    kind: EntryKind,
    size: u64,
    allocated_bytes: u64,
    filesystem_id: u64,
    metadata: Option<SourceMetadata>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceMetadata {
    pub mode: u32,
    pub owner: u32,
    pub group: u32,
    pub accessed_seconds: i64,
    pub accessed_nanoseconds: i64,
    pub modified_seconds: i64,
    pub modified_nanoseconds: i64,
}

impl EntrySnapshot {
    #[must_use]
    pub fn new(
        identity: impl Into<Box<[u8]>>,
        kind: EntryKind,
        size: u64,
        allocated_bytes: u64,
        filesystem_id: u64,
    ) -> Self {
        Self {
            identity: identity.into(),
            kind,
            size,
            allocated_bytes,
            filesystem_id,
            metadata: None,
        }
    }

    #[must_use]
    pub const fn with_metadata(mut self, metadata: SourceMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    #[must_use]
    pub fn identity(&self) -> &[u8] {
        &self.identity
    }

    #[must_use]
    pub const fn kind(&self) -> EntryKind {
        self.kind
    }

    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    #[must_use]
    pub const fn allocated_bytes(&self) -> u64 {
        self.allocated_bytes
    }

    #[must_use]
    pub const fn filesystem_id(&self) -> u64 {
        self.filesystem_id
    }

    #[must_use]
    pub const fn metadata(&self) -> Option<SourceMetadata> {
        self.metadata
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CopyCapabilities {
    pub reflink: bool,
    pub sparse: bool,
    pub hard_links: bool,
    pub atomic_rename: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CopyOptions {
    follow_links: bool,
    include_nested_mounts: bool,
}

impl CopyOptions {
    #[must_use]
    pub const fn follow_links(mut self, follow: bool) -> Self {
        self.follow_links = follow;
        self
    }

    #[must_use]
    pub const fn include_nested_mounts(mut self, include: bool) -> Self {
        self.include_nested_mounts = include;
        self
    }

    #[must_use]
    pub const fn follows_links(self) -> bool {
        self.follow_links
    }

    #[must_use]
    pub const fn includes_nested_mounts(self) -> bool {
        self.include_nested_mounts
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyRequest {
    job_id: JobId,
    generation: EventGeneration,
    source: StorePath,
    destination: StorePath,
    options: CopyOptions,
}

impl CopyRequest {
    #[must_use]
    pub fn new(
        job_id: JobId,
        generation: EventGeneration,
        source: StorePath,
        destination: StorePath,
    ) -> Self {
        Self {
            job_id,
            generation,
            source,
            destination,
            options: CopyOptions::default(),
        }
    }

    #[must_use]
    pub const fn with_options(mut self, options: CopyOptions) -> Self {
        self.options = options;
        self
    }

    #[must_use]
    pub const fn source(&self) -> &StorePath {
        &self.source
    }

    #[must_use]
    pub const fn destination(&self) -> &StorePath {
        &self.destination
    }

    #[must_use]
    pub const fn options(&self) -> CopyOptions {
        self.options
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProviderError {
    Unsupported(Box<str>),
    OutOfSpace,
    PermissionDenied,
    ShortWrite { expected: u64, written: u64 },
    StagingExists,
    SourceChanged,
    NestedMount,
    Cancelled,
    PublishUnknown,
    SourceRemovalUnknown,
    AtomicMoveUnknown,
    Other(Box<str>),
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(reason) => write!(formatter, "operation is unsupported: {reason}"),
            Self::OutOfSpace => formatter.write_str("destination is out of space"),
            Self::PermissionDenied => formatter.write_str("operation was denied"),
            Self::ShortWrite { expected, written } => {
                write!(
                    formatter,
                    "short write: expected {expected} bytes, wrote {written}"
                )
            }
            Self::StagingExists => formatter.write_str("the operation staging path already exists"),
            Self::SourceChanged => formatter.write_str("source changed during the operation"),
            Self::NestedMount => formatter.write_str("recursive work reached a nested mount"),
            Self::Cancelled => formatter.write_str("operation was cancelled"),
            Self::PublishUnknown => {
                formatter.write_str("destination publication has an unknown outcome")
            }
            Self::SourceRemovalUnknown => {
                formatter.write_str("source removal has an unknown outcome")
            }
            Self::AtomicMoveUnknown => formatter.write_str("atomic move has an unknown outcome"),
            Self::Other(message) => formatter.write_str(message),
        }
    }
}

impl Error for ProviderError {}

pub trait CopyProvider {
    fn capabilities(&self, source: &StorePath, destination: &StorePath) -> CopyCapabilities;
    fn inspect(
        &mut self,
        path: &StorePath,
        follow_links: bool,
    ) -> Result<EntrySnapshot, ProviderError>;
    fn create_staging(&mut self, staging: &StorePath, kind: EntryKind)
    -> Result<(), ProviderError>;
    fn try_hard_link(
        &mut self,
        existing: &StorePath,
        staging: &StorePath,
    ) -> Result<bool, ProviderError>;
    fn try_reflink(
        &mut self,
        source: &StorePath,
        staging: &StorePath,
    ) -> Result<bool, ProviderError>;
    fn try_sparse_copy(
        &mut self,
        source: &StorePath,
        staging: &StorePath,
        cancellation: &CancellationToken,
    ) -> Result<Option<u64>, ProviderError>;
    fn copy_streamed(
        &mut self,
        source: &StorePath,
        staging: &StorePath,
        cancellation: &CancellationToken,
    ) -> Result<u64, ProviderError>;
    fn copy_symlink(
        &mut self,
        source: &StorePath,
        staging: &StorePath,
    ) -> Result<(), ProviderError>;
    fn copy_directory(
        &mut self,
        source: &StorePath,
        staging: &StorePath,
        include_nested_mounts: bool,
        cancellation: &CancellationToken,
    ) -> Result<u64, ProviderError>;
    fn apply_metadata(
        &mut self,
        source: &StorePath,
        source_snapshot: &EntrySnapshot,
        staging: &StorePath,
    ) -> Result<MetadataReport, ProviderError>;
    fn verify(
        &mut self,
        source_path: &StorePath,
        source: &EntrySnapshot,
        staging: &StorePath,
        metadata: &MetadataReport,
    ) -> Result<bool, ProviderError>;
    fn publish(
        &mut self,
        staging: &StorePath,
        destination: &StorePath,
        cancellation: &CancellationToken,
    ) -> Result<(), ProviderError>;
    fn cleanup_staging(&mut self, staging: &StorePath) -> Result<(), ProviderError>;
    fn try_atomic_move(
        &mut self,
        source: &StorePath,
        destination: &StorePath,
    ) -> Result<bool, ProviderError>;
    fn remove_source(
        &mut self,
        source: &StorePath,
        expected: &EntrySnapshot,
    ) -> Result<(), ProviderError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyStrategy {
    HardLink,
    Reflink,
    Sparse,
    Streamed,
    SymbolicLink,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyOutcome {
    strategy: CopyStrategy,
    bytes_copied: u64,
    metadata: MetadataReport,
    source_snapshot: EntrySnapshot,
}

impl CopyOutcome {
    #[must_use]
    pub const fn strategy(&self) -> CopyStrategy {
        self.strategy
    }

    #[must_use]
    pub const fn bytes_copied(&self) -> u64 {
        self.bytes_copied
    }

    #[must_use]
    pub const fn metadata(&self) -> &MetadataReport {
        &self.metadata
    }

    pub(crate) const fn source_snapshot(&self) -> &EntrySnapshot {
        &self.source_snapshot
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FailureKind {
    Cancelled,
    UnsupportedSpecialFile(EntryKind),
    StagingUnavailable,
    SourceChanged,
    VerificationFailed,
    Provider(ProviderError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationState {
    NotPublished,
    Published,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceState {
    Retained,
    Removed,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationFailure {
    kind: FailureKind,
    staging_retained: Option<StorePath>,
    destination: StorePath,
    publication_state: PublicationState,
    source_state: SourceState,
}

impl OperationFailure {
    pub(crate) fn before_publish(kind: FailureKind, destination: &StorePath) -> Self {
        Self {
            kind,
            staging_retained: None,
            destination: destination.clone(),
            publication_state: PublicationState::NotPublished,
            source_state: SourceState::Retained,
        }
    }

    pub(crate) fn after_publish(kind: FailureKind, destination: &StorePath) -> Self {
        Self {
            kind,
            staging_retained: None,
            destination: destination.clone(),
            publication_state: PublicationState::Published,
            source_state: SourceState::Retained,
        }
    }

    pub(crate) fn after_source_removal_unknown(kind: FailureKind, destination: &StorePath) -> Self {
        Self {
            kind,
            staging_retained: None,
            destination: destination.clone(),
            publication_state: PublicationState::Published,
            source_state: SourceState::Unknown,
        }
    }

    pub(crate) fn atomic_move_unknown(kind: FailureKind, destination: &StorePath) -> Self {
        Self {
            kind,
            staging_retained: None,
            destination: destination.clone(),
            publication_state: PublicationState::Unknown,
            source_state: SourceState::Unknown,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> &FailureKind {
        &self.kind
    }

    #[must_use]
    pub const fn staging_retained(&self) -> Option<&StorePath> {
        self.staging_retained.as_ref()
    }

    #[must_use]
    pub const fn destination(&self) -> &StorePath {
        &self.destination
    }

    #[must_use]
    pub const fn destination_published(&self) -> bool {
        matches!(self.publication_state, PublicationState::Published)
    }

    #[must_use]
    pub const fn publication_state(&self) -> PublicationState {
        self.publication_state
    }

    #[must_use]
    pub const fn source_retained(&self) -> bool {
        matches!(self.source_state, SourceState::Retained)
    }

    #[must_use]
    pub const fn source_state(&self) -> SourceState {
        self.source_state
    }
}

impl fmt::Display for OperationFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "operation failed: {:?}", self.kind)
    }
}

impl Error for OperationFailure {}

#[derive(Default)]
pub struct CopySession {
    copied_identities: BTreeMap<Box<[u8]>, StorePath>,
}

impl CopySession {
    pub fn execute<P: CopyProvider>(
        &mut self,
        provider: &mut P,
        request: &CopyRequest,
        cancellation: &CancellationToken,
    ) -> Result<CopyOutcome, OperationFailure> {
        check_cancellation(cancellation, request.destination())?;
        let source = provider
            .inspect(request.source(), request.options.follows_links())
            .map_err(|error| provider_failure(error, request.destination()))?;
        if !source.kind.copyable() {
            return Err(OperationFailure::before_publish(
                FailureKind::UnsupportedSpecialFile(source.kind),
                request.destination(),
            ));
        }

        let staging =
            StagingPath::for_destination(request.destination(), request.job_id, request.generation)
                .map_err(|_| {
                    OperationFailure::before_publish(
                        FailureKind::StagingUnavailable,
                        request.destination(),
                    )
                })?
                .path()
                .clone();
        if let Err(error) = provider.create_staging(&staging, source.kind) {
            return Err(match error {
                ProviderError::StagingExists => OperationFailure::before_publish(
                    FailureKind::Provider(ProviderError::StagingExists),
                    request.destination(),
                ),
                error => cleanup_failure(
                    provider,
                    provider_failure_kind(error),
                    request.destination(),
                    &staging,
                ),
            });
        }

        let repeated_identity = self.copied_identities.contains_key(source.identity());
        let result = self.copy_to_staging(provider, request, &source, &staging, cancellation);
        let (strategy, bytes_copied) = match result {
            Ok(result) => result,
            Err(kind) => {
                return Err(cleanup_failure(
                    provider,
                    kind,
                    request.destination(),
                    &staging,
                ));
            }
        };
        check_cancellation(cancellation, request.destination()).map_err(|failure| {
            cleanup_failure(provider, failure.kind, request.destination(), &staging)
        })?;
        let mut metadata = provider
            .apply_metadata(request.source(), &source, &staging)
            .map_err(|error| {
                cleanup_failure(
                    provider,
                    provider_failure_kind(error),
                    request.destination(),
                    &staging,
                )
            })?;
        if source.allocated_bytes < source.size
            && !matches!(strategy, CopyStrategy::Reflink | CopyStrategy::Sparse)
        {
            metadata.note_skipped(MetadataKind::SparseLayout);
        }
        if repeated_identity && strategy != CopyStrategy::HardLink {
            metadata.note_skipped(MetadataKind::HardLinkRelationship);
        }
        let after = provider
            .inspect(request.source(), request.options.follows_links())
            .map_err(|error| {
                cleanup_failure(
                    provider,
                    provider_failure_kind(error),
                    request.destination(),
                    &staging,
                )
            })?;
        if !source_unchanged(&source, &after) {
            return Err(cleanup_failure(
                provider,
                FailureKind::SourceChanged,
                request.destination(),
                &staging,
            ));
        }
        let verified = provider
            .verify(request.source(), &source, &staging, &metadata)
            .map_err(|error| {
                cleanup_failure(
                    provider,
                    provider_failure_kind(error),
                    request.destination(),
                    &staging,
                )
            })?;
        if !verified {
            return Err(cleanup_failure(
                provider,
                FailureKind::VerificationFailed,
                request.destination(),
                &staging,
            ));
        }
        check_cancellation(cancellation, request.destination()).map_err(|failure| {
            cleanup_failure(provider, failure.kind, request.destination(), &staging)
        })?;
        if let Err(error) = provider.publish(&staging, request.destination(), cancellation) {
            return Err(publish_failure(
                provider,
                error,
                request.destination(),
                &staging,
            ));
        }

        self.copied_identities
            .insert(source.identity.clone(), request.destination().clone());
        Ok(CopyOutcome {
            strategy,
            bytes_copied,
            metadata,
            source_snapshot: source,
        })
    }

    fn copy_to_staging<P: CopyProvider>(
        &self,
        provider: &mut P,
        request: &CopyRequest,
        source: &EntrySnapshot,
        staging: &StorePath,
        cancellation: &CancellationToken,
    ) -> Result<(CopyStrategy, u64), FailureKind> {
        let capabilities = provider.capabilities(request.source(), request.destination());
        match source.kind {
            EntryKind::SymbolicLink => {
                provider
                    .copy_symlink(request.source(), staging)
                    .map_err(provider_failure_kind)?;
                Ok((CopyStrategy::SymbolicLink, 0))
            }
            EntryKind::Directory => provider
                .copy_directory(
                    request.source(),
                    staging,
                    request.options.includes_nested_mounts(),
                    cancellation,
                )
                .map(|bytes| (CopyStrategy::Directory, bytes))
                .map_err(provider_failure_kind),
            EntryKind::RegularFile => {
                if capabilities.hard_links
                    && let Some(existing) = self.copied_identities.get(source.identity())
                    && provider
                        .try_hard_link(existing, staging)
                        .map_err(provider_failure_kind)?
                {
                    return Ok((CopyStrategy::HardLink, source.size));
                }
                if capabilities.reflink
                    && provider
                        .try_reflink(request.source(), staging)
                        .map_err(provider_failure_kind)?
                {
                    return Ok((CopyStrategy::Reflink, source.size));
                }
                if capabilities.sparse
                    && source.allocated_bytes < source.size
                    && let Some(bytes) = provider
                        .try_sparse_copy(request.source(), staging, cancellation)
                        .map_err(provider_failure_kind)?
                {
                    return Ok((CopyStrategy::Sparse, bytes));
                }
                provider
                    .copy_streamed(request.source(), staging, cancellation)
                    .map(|bytes| (CopyStrategy::Streamed, bytes))
                    .map_err(provider_failure_kind)
            }
            EntryKind::BlockDevice
            | EntryKind::CharacterDevice
            | EntryKind::Fifo
            | EntryKind::Socket => Err(FailureKind::UnsupportedSpecialFile(source.kind)),
        }
    }
}

fn check_cancellation(
    cancellation: &CancellationToken,
    destination: &StorePath,
) -> Result<(), OperationFailure> {
    cancellation.check().map_err(|error| match error {
        StoreError::Cancelled => {
            OperationFailure::before_publish(FailureKind::Cancelled, destination)
        }
        _ => OperationFailure::before_publish(
            FailureKind::Provider(ProviderError::Other(error.to_string().into())),
            destination,
        ),
    })
}

fn provider_failure(error: ProviderError, destination: &StorePath) -> OperationFailure {
    OperationFailure::before_publish(provider_failure_kind(error), destination)
}

pub(crate) fn provider_failure_kind(error: ProviderError) -> FailureKind {
    match error {
        ProviderError::Cancelled => FailureKind::Cancelled,
        ProviderError::SourceChanged => FailureKind::SourceChanged,
        error => FailureKind::Provider(error),
    }
}

fn cleanup_failure<P: CopyProvider>(
    provider: &mut P,
    kind: FailureKind,
    destination: &StorePath,
    staging: &StorePath,
) -> OperationFailure {
    let staging_retained = provider
        .cleanup_staging(staging)
        .err()
        .map(|_| staging.clone());
    OperationFailure {
        kind,
        staging_retained,
        destination: destination.clone(),
        publication_state: PublicationState::NotPublished,
        source_state: SourceState::Retained,
    }
}

fn publish_failure<P: CopyProvider>(
    provider: &mut P,
    error: ProviderError,
    destination: &StorePath,
    staging: &StorePath,
) -> OperationFailure {
    let unknown = error == ProviderError::PublishUnknown;
    let mut failure = cleanup_failure(provider, provider_failure_kind(error), destination, staging);
    if unknown {
        failure.publication_state = PublicationState::Unknown;
    }
    failure
}
