use crate::JobState;
use musheen_core::{
    CapabilityKind, CapabilityMatrix, CapabilityState, ItemId, ProviderId, StorePath,
};
use std::error::Error;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WorkClass {
    DataMutation,
    Metadata,
    HashOrPreview,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OperationKind {
    Copy,
    Move,
    Trash,
    PermanentDelete,
    CreateFile,
    CreateDirectory,
    Rename,
    SymbolicLink,
    HardLink,
    SetPermissions,
    SetOwnership,
    SetExtendedAttribute,
    Compress,
    Extract,
    Restore,
    Hash,
    Preview,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ArchiveCodec {
    Zip,
    Tar,
    TarGzip,
    TarZstd,
    SevenZip,
}

impl ArchiveCodec {
    #[must_use]
    pub const fn supports_encryption(self) -> bool {
        matches!(self, Self::Zip | Self::SevenZip)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ArchiveConflictPolicy {
    Fail,
    Skip,
    Replace,
}

/// A validated archive request submitted to the operation engine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveOperationPlan {
    kind: OperationKind,
    sources: Vec<StorePath>,
    destination: StorePath,
    codec: ArchiveCodec,
    conflict_policy: ArchiveConflictPolicy,
    encrypted: bool,
}

impl ArchiveOperationPlan {
    pub fn create(
        sources: Vec<StorePath>,
        destination: StorePath,
        codec: ArchiveCodec,
        conflict_policy: ArchiveConflictPolicy,
        encrypted: bool,
    ) -> Result<Self, PlanError> {
        if sources.is_empty() {
            return Err(PlanError::EmptyArchiveSources);
        }
        Self::new(
            OperationKind::Compress,
            sources,
            destination,
            codec,
            conflict_policy,
            encrypted,
        )
    }

    pub fn extract(
        source: StorePath,
        destination: StorePath,
        codec: ArchiveCodec,
        conflict_policy: ArchiveConflictPolicy,
        encrypted: bool,
    ) -> Result<Self, PlanError> {
        Self::new(
            OperationKind::Extract,
            vec![source],
            destination,
            codec,
            conflict_policy,
            encrypted,
        )
    }

    fn new(
        kind: OperationKind,
        sources: Vec<StorePath>,
        destination: StorePath,
        codec: ArchiveCodec,
        conflict_policy: ArchiveConflictPolicy,
        encrypted: bool,
    ) -> Result<Self, PlanError> {
        if encrypted && !codec.supports_encryption() {
            return Err(PlanError::UnsupportedArchiveEncryption(codec));
        }
        if sources.iter().any(|source| source == &destination) {
            return Err(PlanError::ArchiveSourceIsDestination);
        }
        Ok(Self {
            kind,
            sources,
            destination,
            codec,
            conflict_policy,
            encrypted,
        })
    }

    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        self.kind
    }

    #[must_use]
    pub fn sources(&self) -> &[StorePath] {
        &self.sources
    }

    #[must_use]
    pub const fn destination(&self) -> &StorePath {
        &self.destination
    }

    #[must_use]
    pub const fn codec(&self) -> ArchiveCodec {
        self.codec
    }

    #[must_use]
    pub const fn conflict_policy(&self) -> ArchiveConflictPolicy {
        self.conflict_policy
    }

    #[must_use]
    pub const fn encrypted(&self) -> bool {
        self.encrypted
    }
}

impl OperationKind {
    #[must_use]
    pub const fn class(self) -> WorkClass {
        match self {
            Self::Copy
            | Self::Move
            | Self::Trash
            | Self::PermanentDelete
            | Self::Compress
            | Self::Extract
            | Self::Restore => WorkClass::DataMutation,
            Self::CreateFile
            | Self::CreateDirectory
            | Self::Rename
            | Self::SymbolicLink
            | Self::HardLink
            | Self::SetPermissions
            | Self::SetOwnership
            | Self::SetExtendedAttribute => WorkClass::Metadata,
            Self::Hash | Self::Preview => WorkClass::HashOrPreview,
        }
    }

    #[must_use]
    pub const fn requires_source(self) -> bool {
        matches!(
            self,
            Self::Copy
                | Self::Move
                | Self::Rename
                | Self::SymbolicLink
                | Self::HardLink
                | Self::Compress
                | Self::Extract
                | Self::Restore
                | Self::Hash
                | Self::Preview
        )
    }

    const fn source_is_mutated(self) -> bool {
        matches!(
            self,
            Self::Move | Self::Rename | Self::Trash | Self::PermanentDelete
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderLimits {
    data_mutations: usize,
    metadata_jobs: usize,
    hash_preview_jobs: usize,
}

impl ProviderLimits {
    pub fn new(
        data_mutations: usize,
        metadata_jobs: usize,
        hash_preview_jobs: usize,
    ) -> Result<Self, PlanError> {
        for (class, value) in [
            (WorkClass::DataMutation, data_mutations),
            (WorkClass::Metadata, metadata_jobs),
            (WorkClass::HashOrPreview, hash_preview_jobs),
        ] {
            if value == 0 {
                return Err(PlanError::InvalidProviderLimit { class, value });
            }
        }
        Ok(Self {
            data_mutations,
            metadata_jobs,
            hash_preview_jobs,
        })
    }

    #[must_use]
    pub const fn unbounded() -> Self {
        Self {
            data_mutations: usize::MAX,
            metadata_jobs: usize::MAX,
            hash_preview_jobs: usize::MAX,
        }
    }

    #[must_use]
    pub const fn for_class(&self, class: WorkClass) -> usize {
        match class {
            WorkClass::DataMutation => self.data_mutations,
            WorkClass::Metadata => self.metadata_jobs,
            WorkClass::HashOrPreview => self.hash_preview_jobs,
        }
    }
}

impl Default for ProviderLimits {
    fn default() -> Self {
        Self::unbounded()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderSnapshot {
    id: ProviderId,
    capabilities: CapabilityMatrix,
    limits: ProviderLimits,
}

impl ProviderSnapshot {
    #[must_use]
    pub const fn new(
        id: ProviderId,
        capabilities: CapabilityMatrix,
        limits: ProviderLimits,
    ) -> Self {
        Self {
            id,
            capabilities,
            limits,
        }
    }

    #[must_use]
    pub const fn id(&self) -> &ProviderId {
        &self.id
    }

    #[must_use]
    pub const fn capabilities(&self) -> &CapabilityMatrix {
        &self.capabilities
    }

    #[must_use]
    pub const fn limits(&self) -> &ProviderLimits {
        &self.limits
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationPlan {
    kind: OperationKind,
    provider: ProviderSnapshot,
    source: Option<StorePath>,
    destination: StorePath,
    inverse: Option<Box<InverseTemplate>>,
}

impl OperationPlan {
    pub fn new(
        kind: OperationKind,
        provider: ProviderSnapshot,
        source: Option<StorePath>,
        destination: StorePath,
    ) -> Result<Self, PlanError> {
        if kind.requires_source() && source.is_none() {
            return Err(PlanError::MissingSource(kind));
        }
        if !kind.requires_source() && source.is_some() {
            return Err(PlanError::UnexpectedSource(kind));
        }
        Ok(Self {
            kind,
            provider,
            source,
            destination,
            inverse: None,
        })
    }

    #[must_use]
    pub fn with_inverse(mut self, inverse: InverseTemplate) -> Self {
        self.inverse = Some(Box::new(inverse));
        self
    }

    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        self.kind
    }

    #[must_use]
    pub const fn class(&self) -> WorkClass {
        self.kind.class()
    }

    #[must_use]
    pub const fn provider(&self) -> &ProviderSnapshot {
        &self.provider
    }

    #[must_use]
    pub const fn source(&self) -> Option<&StorePath> {
        self.source.as_ref()
    }

    #[must_use]
    pub const fn destination(&self) -> &StorePath {
        &self.destination
    }

    #[must_use]
    pub fn validated_inverse(
        &self,
        state: JobState,
        current_identity: Option<&ItemId>,
        current_provider: &ProviderId,
        current_capabilities: &CapabilityMatrix,
    ) -> Option<Self> {
        let inverse = self.inverse.as_deref()?;
        if state != JobState::Completed
            || current_provider != self.provider.id()
            || current_identity != Some(&inverse.expected_identity)
            || inverse.plan.provider.id() != current_provider
        {
            return None;
        }
        if let Some(required) = inverse.required_capability
            && !matches!(
                current_capabilities.get(required),
                CapabilityState::Supported
            )
        {
            return None;
        }
        Some(inverse.plan.as_ref().clone())
    }

    pub(crate) fn conflicts_with(&self, other: &Self) -> bool {
        if self.provider.id() != other.provider.id() {
            return false;
        }
        let self_reads = self.read_set();
        let self_writes = self.write_set();
        let other_reads = other.read_set();
        let other_writes = other.write_set();
        self_writes.iter().flatten().any(|write| {
            other_reads
                .iter()
                .chain(other_writes.iter())
                .flatten()
                .any(|path| paths_overlap(write, path))
        }) || other_writes.iter().flatten().any(|write| {
            self_reads
                .iter()
                .flatten()
                .any(|path| paths_overlap(write, path))
        })
    }

    fn read_set(&self) -> [Option<&StorePath>; 1] {
        [self.source.as_ref()]
    }

    fn write_set(&self) -> [Option<&StorePath>; 2] {
        if self.class() == WorkClass::HashOrPreview {
            return [None, None];
        }
        [
            Some(&self.destination),
            self.kind
                .source_is_mutated()
                .then_some(self.source.as_ref())
                .flatten(),
        ]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InverseTemplate {
    plan: Box<OperationPlan>,
    expected_identity: ItemId,
    required_capability: Option<CapabilityKind>,
}

impl InverseTemplate {
    #[must_use]
    pub fn new(
        plan: OperationPlan,
        expected_identity: ItemId,
        required_capability: Option<CapabilityKind>,
    ) -> Self {
        Self {
            plan: Box::new(plan),
            expected_identity,
            required_capability,
        }
    }
}

fn paths_overlap(left: &StorePath, right: &StorePath) -> bool {
    match (left.as_unix_path(), right.as_unix_path()) {
        (Some(left), Some(right)) => {
            left == right || left.starts_with(right) || right.starts_with(left)
        }
        _ => left == right,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanError {
    MissingSource(OperationKind),
    UnexpectedSource(OperationKind),
    InvalidProviderLimit { class: WorkClass, value: usize },
    EmptyArchiveSources,
    ArchiveSourceIsDestination,
    UnsupportedArchiveEncryption(ArchiveCodec),
}

impl fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSource(kind) => write!(formatter, "{kind:?} requires a source"),
            Self::UnexpectedSource(kind) => write!(formatter, "{kind:?} does not accept a source"),
            Self::InvalidProviderLimit { class, value } => {
                write!(
                    formatter,
                    "{class:?} provider limit must be positive, got {value}"
                )
            }
            Self::EmptyArchiveSources => formatter.write_str("archive creation requires a source"),
            Self::ArchiveSourceIsDestination => {
                formatter.write_str("archive source and destination must differ")
            }
            Self::UnsupportedArchiveEncryption(codec) => {
                write!(formatter, "{codec:?} does not support archive encryption")
            }
        }
    }
}

impl Error for PlanError {}
