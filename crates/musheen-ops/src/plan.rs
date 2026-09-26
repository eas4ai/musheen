use crate::JobState;
use musheen_core::{
    CapabilityKind, CapabilityMatrix, CapabilityState, ItemId, ProviderId, StorePath,
};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WorkClass {
    DataMutation,
    Metadata,
    HashOrPreview,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum ArchiveConflictPolicy {
    Fail,
    Skip,
    Replace,
}

/// An existing item the user answered about, as it was when they answered:
/// its device, inode and kind. An answer holds only while the item at its
/// path is still this one.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AnsweredItem {
    pub device: u64,
    pub inode: u64,
    pub folder: bool,
}

impl AnsweredItem {
    /// Whether the item with this device and inode is the one answered about.
    #[must_use]
    pub const fn is(&self, device: u64, inode: u64) -> bool {
        self.device == device && self.inode == inode
    }
}

/// How an extraction merges into a destination folder that already exists.
/// It names, relative to that folder, each colliding item the user chose to
/// replace, as it was when they answered; every other collision is skipped,
/// including one that appeared or changed after the user answered. When
/// something other than a folder has the destination's name, `whole` names
/// the item the user chose to replace, and only that item is replaced.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExtractMerge {
    #[serde(with = "answer_list")]
    replace: std::collections::BTreeMap<Vec<u8>, AnsweredItem>,
    whole: Option<AnsweredItem>,
}

/// Writes the answers as a list of path and item pairs: a JSON map key must
/// be a string, and an entry path is bytes.
mod answer_list {
    use super::AnsweredItem;
    use serde::{Deserialize, Deserializer, Serializer};
    use std::collections::BTreeMap;

    pub(super) fn serialize<S: Serializer>(
        answers: &BTreeMap<Vec<u8>, AnsweredItem>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(answers)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<Vec<u8>, AnsweredItem>, D::Error> {
        Ok(Vec::<(Vec<u8>, AnsweredItem)>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

impl ExtractMerge {
    /// `replace` maps entry paths relative to the destination folder, with
    /// `/` between components, to the items the user chose to replace.
    pub fn new(replace: impl IntoIterator<Item = (Vec<u8>, AnsweredItem)>) -> Self {
        Self {
            replace: replace.into_iter().collect(),
            whole: None,
        }
    }

    /// Replaces the item that has the destination folder's name, while it is
    /// still `item`.
    #[must_use]
    pub fn replacing_destination(item: AnsweredItem) -> Self {
        Self {
            replace: std::collections::BTreeMap::new(),
            whole: Some(item),
        }
    }

    /// Whether the user chose to replace the item at `relative`, and it is
    /// still the item with this device and inode.
    #[must_use]
    pub fn replaces(&self, relative: &[u8], device: u64, inode: u64) -> bool {
        self.replace
            .get(relative)
            .is_some_and(|item| item.is(device, inode))
    }

    /// The item with the destination folder's name the user chose to replace.
    #[must_use]
    pub const fn whole(&self) -> Option<&AnsweredItem> {
        self.whole.as_ref()
    }
}

/// A validated archive request submitted to the operation engine.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArchiveOperationPlan {
    kind: OperationKind,
    sources: Vec<StorePath>,
    destination: StorePath,
    codec: ArchiveCodec,
    conflict_policy: ArchiveConflictPolicy,
    encrypted: bool,
    /// Set when an extraction merges into an existing folder. Journals
    /// written before merging existed read as `None`.
    #[serde(default)]
    merge: Option<ExtractMerge>,
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
            merge: None,
        })
    }

    /// Makes an extraction merge into its destination folder when that
    /// folder exists, following `merge` for each collision.
    pub fn with_merge(mut self, merge: ExtractMerge) -> Result<Self, PlanError> {
        if self.kind != OperationKind::Extract {
            return Err(PlanError::MergeNeedsExtraction);
        }
        self.merge = Some(merge);
        Ok(self)
    }

    #[must_use]
    pub const fn merge(&self) -> Option<&ExtractMerge> {
        self.merge.as_ref()
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

    /// Converts this archive request into a normal operation-engine plan.
    pub fn into_operation_plan(
        self,
        provider: ProviderSnapshot,
    ) -> Result<OperationPlan, PlanError> {
        OperationPlan::from_archive(provider, self)
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
    archive: Option<Box<ArchiveOperationPlan>>,
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
            archive: None,
        })
    }

    pub fn from_archive(
        provider: ProviderSnapshot,
        archive: ArchiveOperationPlan,
    ) -> Result<Self, PlanError> {
        let source = archive.sources.first().cloned();
        let mut plan = Self::new(archive.kind, provider, source, archive.destination.clone())?;
        plan.archive = Some(Box::new(archive));
        Ok(plan)
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
    pub fn archive(&self) -> Option<&ArchiveOperationPlan> {
        self.archive.as_deref()
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
        self_writes.iter().any(|write| {
            other_reads
                .iter()
                .chain(other_writes.iter())
                .any(|path| paths_overlap(write, path))
        }) || other_writes
            .iter()
            .any(|write| self_reads.iter().any(|path| paths_overlap(write, path)))
    }

    fn read_set(&self) -> Vec<&StorePath> {
        if let Some(archive) = self.archive() {
            return archive.sources.iter().collect();
        }
        self.source.iter().collect()
    }

    fn write_set(&self) -> Vec<&StorePath> {
        if self.class() == WorkClass::HashOrPreview {
            return Vec::new();
        }
        let mut writes = vec![&self.destination];
        if self.kind.source_is_mutated()
            && let Some(source) = &self.source
        {
            writes.push(source);
        }
        writes
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
    MergeNeedsExtraction,
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
            Self::MergeNeedsExtraction => {
                formatter.write_str("only an extraction merges into an existing folder")
            }
            Self::UnsupportedArchiveEncryption(codec) => {
                write!(formatter, "{codec:?} does not support archive encryption")
            }
        }
    }
}

impl Error for PlanError {}

#[cfg(test)]
mod extract_merge_tests {
    use super::*;

    #[test]
    fn extract_merge_answers_round_trip_through_the_journal_format() {
        let merge = ExtractMerge::new([(
            b"docs/a.txt".to_vec(),
            AnsweredItem {
                device: 1,
                inode: 2,
                folder: false,
            },
        )]);
        let text = serde_json::to_string(&merge).expect("a merge serializes as JSON");
        assert_eq!(
            serde_json::from_str::<ExtractMerge>(&text).expect("the JSON reads back"),
            merge
        );
        assert!(merge.replaces(b"docs/a.txt", 1, 2));
        assert!(
            !merge.replaces(b"docs/a.txt", 1, 3),
            "another item at the path"
        );
    }
}
