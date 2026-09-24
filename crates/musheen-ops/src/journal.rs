use crate::{ArchiveOperationPlan, EventGeneration, JobId};
use musheen_core::StorePath;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::io;
use std::sync::Arc;

pub const JOURNAL_SCHEMA_VERSION: u32 = 1;
const DEFAULT_IDENTITY_MEMORY_LIMIT: u64 = 512 * 1_024 * 1_024;
const DEFAULT_IDENTITY_TIMEOUT_MILLIS: u64 = 30_000;

const fn default_identity_memory_limit() -> u64 {
    DEFAULT_IDENTITY_MEMORY_LIMIT
}

const fn default_identity_timeout_millis() -> u64 {
    DEFAULT_IDENTITY_TIMEOUT_MILLIS
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalPhase {
    Planned,
    StagingCreated,
    DataCopied,
    MetadataApplied,
    DestinationQuarantinePlanned,
    DestinationQuarantined,
    StagePublishPlanned,
    PrepublishStageCleanupPlanned,
    PublishedDestinationCleanupPlanned,
    DestinationPublished,
    PublishRollbackPlanned,
    PublishedPayloadQuarantined,
    DestinationRestorePlanned,
    DestinationRestored,
    StageRestorePlanned,
    PrepublishStageCleanupQuarantined,
    PublishedDestinationCleanupQuarantined,
    SourceRemoved,
    StagingCleaned,
    RecoveryRequired,
    Completed,
    RolledBack,
}

/// Identifies which owned object a durable cleanup checkpoint may remove.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveCleanupKind {
    /// An unpublished staging payload. Successful cleanup ends in `RolledBack`.
    PrepublishStage,
    /// A displaced destination after its replacement was published.
    PublishedDestination,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "level", content = "reason")]
pub enum Durability {
    CrashDurable,
    BestEffort(Box<str>),
}

/// Stable filesystem identity recorded at an archive recovery boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArchivePathIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    directory: bool,
    #[serde(default)]
    content_digest: [u8; 32],
}

impl ArchivePathIdentity {
    #[must_use]
    pub const fn new(
        device: u64,
        inode: u64,
        size: u64,
        modified_seconds: i64,
        modified_nanoseconds: i64,
        directory: bool,
    ) -> Self {
        Self {
            device,
            inode,
            size,
            modified_seconds,
            modified_nanoseconds,
            directory,
            content_digest: [0; 32],
        }
    }

    #[must_use]
    pub const fn with_content_digest(mut self, content_digest: [u8; 32]) -> Self {
        self.content_digest = content_digest;
        self
    }

    #[must_use]
    pub const fn device(self) -> u64 {
        self.device
    }
    #[must_use]
    pub const fn inode(self) -> u64 {
        self.inode
    }
    #[must_use]
    pub const fn size(self) -> u64 {
        self.size
    }
    #[must_use]
    pub const fn modified_seconds(self) -> i64 {
        self.modified_seconds
    }
    #[must_use]
    pub const fn modified_nanoseconds(self) -> i64 {
        self.modified_nanoseconds
    }
    #[must_use]
    pub const fn is_directory(self) -> bool {
        self.directory
    }

    #[must_use]
    pub const fn content_digest(self) -> [u8; 32] {
        self.content_digest
    }
}

/// Durable inputs and identities needed to recover one archive operation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArchiveCheckpoint {
    plan: Arc<ArchiveOperationPlan>,
    #[serde(default)]
    plan_digest: [u8; 32],
    staging: StorePath,
    staging_identity: Option<ArchivePathIdentity>,
    destination_before: Option<ArchivePathIdentity>,
    destination_after: Option<ArchivePathIdentity>,
    #[serde(default)]
    staging_nonce: Option<[u8; 16]>,
    #[serde(default)]
    cleanup: Option<StorePath>,
    #[serde(default)]
    cleanup_kind: Option<ArchiveCleanupKind>,
    #[serde(default)]
    cleanup_identity: Option<ArchivePathIdentity>,
    #[serde(default)]
    cleanup_deletion: Option<StorePath>,
    #[serde(default)]
    stage_deletion: Option<StorePath>,
    #[serde(default)]
    publication_quarantine: Option<StorePath>,
    #[serde(default = "default_identity_memory_limit")]
    identity_memory_limit: u64,
    #[serde(default = "default_identity_timeout_millis")]
    identity_timeout_millis: u64,
}

impl ArchiveCheckpoint {
    #[must_use]
    pub fn new(
        plan: ArchiveOperationPlan,
        staging: StorePath,
        staging_identity: Option<ArchivePathIdentity>,
        destination_before: Option<ArchivePathIdentity>,
        destination_after: Option<ArchivePathIdentity>,
    ) -> Self {
        let plan_digest = serde_json::to_vec(&plan)
            .map(|bytes| *blake3::hash(&bytes).as_bytes())
            .unwrap_or([0; 32]);
        Self {
            plan: Arc::new(plan),
            plan_digest,
            staging,
            staging_identity,
            destination_before,
            destination_after,
            staging_nonce: None,
            cleanup: None,
            cleanup_kind: None,
            cleanup_identity: None,
            cleanup_deletion: None,
            stage_deletion: None,
            publication_quarantine: None,
            identity_memory_limit: DEFAULT_IDENTITY_MEMORY_LIMIT,
            identity_timeout_millis: DEFAULT_IDENTITY_TIMEOUT_MILLIS,
        }
    }

    #[must_use]
    pub const fn with_staging_nonce(mut self, staging_nonce: [u8; 16]) -> Self {
        self.staging_nonce = Some(staging_nonce);
        self
    }

    #[must_use]
    pub fn with_cleanup_intent(
        mut self,
        kind: ArchiveCleanupKind,
        source: StorePath,
        quarantine: StorePath,
        identity: Option<ArchivePathIdentity>,
    ) -> Self {
        self.cleanup_kind = Some(kind);
        self.cleanup = Some(source);
        self.cleanup_deletion = Some(quarantine);
        self.cleanup_identity = identity;
        self
    }

    #[must_use]
    pub fn with_stage_deletion(mut self, stage_deletion: StorePath) -> Self {
        self.stage_deletion = Some(stage_deletion);
        self
    }

    #[must_use]
    pub fn with_publication_quarantine(mut self, publication_quarantine: StorePath) -> Self {
        self.publication_quarantine = Some(publication_quarantine);
        self
    }

    #[must_use]
    pub const fn with_identity_memory_limit(mut self, limit: u64) -> Self {
        self.identity_memory_limit = limit;
        self
    }

    #[must_use]
    pub const fn with_identity_timeout_millis(mut self, timeout: u64) -> Self {
        self.identity_timeout_millis = timeout;
        self
    }

    #[must_use]
    pub fn plan(&self) -> &ArchiveOperationPlan {
        self.plan.as_ref()
    }

    #[must_use]
    pub fn shared_plan(&self) -> Arc<ArchiveOperationPlan> {
        Arc::clone(&self.plan)
    }

    #[must_use]
    pub const fn plan_digest(&self) -> [u8; 32] {
        self.plan_digest
    }

    #[must_use]
    pub fn from_shared_plan(
        plan: Arc<ArchiveOperationPlan>,
        plan_digest: [u8; 32],
        staging: StorePath,
        staging_identity: Option<ArchivePathIdentity>,
        destination_before: Option<ArchivePathIdentity>,
        destination_after: Option<ArchivePathIdentity>,
    ) -> Self {
        Self {
            plan,
            plan_digest,
            staging,
            staging_identity,
            destination_before,
            destination_after,
            staging_nonce: None,
            cleanup: None,
            cleanup_kind: None,
            cleanup_identity: None,
            cleanup_deletion: None,
            stage_deletion: None,
            publication_quarantine: None,
            identity_memory_limit: DEFAULT_IDENTITY_MEMORY_LIMIT,
            identity_timeout_millis: DEFAULT_IDENTITY_TIMEOUT_MILLIS,
        }
    }
    #[must_use]
    pub const fn staging(&self) -> &StorePath {
        &self.staging
    }
    #[must_use]
    pub const fn staging_identity(&self) -> Option<ArchivePathIdentity> {
        self.staging_identity
    }
    #[must_use]
    pub const fn destination_before(&self) -> Option<ArchivePathIdentity> {
        self.destination_before
    }
    #[must_use]
    pub const fn destination_after(&self) -> Option<ArchivePathIdentity> {
        self.destination_after
    }
    #[must_use]
    pub const fn staging_nonce(&self) -> Option<[u8; 16]> {
        self.staging_nonce
    }
    #[must_use]
    pub const fn cleanup(&self) -> Option<&StorePath> {
        self.cleanup.as_ref()
    }
    #[must_use]
    pub const fn cleanup_kind(&self) -> Option<ArchiveCleanupKind> {
        self.cleanup_kind
    }
    #[must_use]
    pub const fn cleanup_identity(&self) -> Option<ArchivePathIdentity> {
        self.cleanup_identity
    }
    #[must_use]
    pub const fn cleanup_deletion(&self) -> Option<&StorePath> {
        self.cleanup_deletion.as_ref()
    }
    #[must_use]
    pub const fn stage_deletion(&self) -> Option<&StorePath> {
        self.stage_deletion.as_ref()
    }
    #[must_use]
    pub const fn publication_quarantine(&self) -> Option<&StorePath> {
        self.publication_quarantine.as_ref()
    }
    #[must_use]
    pub const fn identity_memory_limit(&self) -> u64 {
        self.identity_memory_limit
    }
    #[must_use]
    pub const fn identity_timeout_millis(&self) -> u64 {
        self.identity_timeout_millis
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalRecord {
    schema_version: u32,
    sequence: u64,
    job_id: JobId,
    generation: EventGeneration,
    phase: JournalPhase,
    durability: Durability,
    archive: Option<ArchiveCheckpoint>,
}

impl JournalRecord {
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub const fn job_id(&self) -> JobId {
        self.job_id
    }

    #[must_use]
    pub const fn generation(&self) -> EventGeneration {
        self.generation
    }

    #[must_use]
    pub const fn phase(&self) -> JournalPhase {
        self.phase
    }

    #[must_use]
    pub const fn durability(&self) -> &Durability {
        &self.durability
    }

    #[must_use]
    pub const fn archive_checkpoint(&self) -> Option<&ArchiveCheckpoint> {
        self.archive.as_ref()
    }
}

#[derive(Deserialize, Serialize)]
struct RecordDocument {
    schema_version: u32,
    sequence: u64,
    job_id: u64,
    generation: u64,
    phase: JournalPhase,
    durability: Durability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    archive: Option<ArchiveCheckpointDocument>,
}

#[derive(Deserialize, Serialize)]
struct ArchiveCheckpointDocument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan: Option<ArchiveOperationPlan>,
    #[serde(default)]
    plan_digest: [u8; 32],
    staging: StorePath,
    staging_identity: Option<ArchivePathIdentity>,
    destination_before: Option<ArchivePathIdentity>,
    destination_after: Option<ArchivePathIdentity>,
    #[serde(default)]
    staging_nonce: Option<[u8; 16]>,
    #[serde(default)]
    cleanup: Option<StorePath>,
    #[serde(default)]
    cleanup_kind: Option<ArchiveCleanupKind>,
    #[serde(default)]
    cleanup_identity: Option<ArchivePathIdentity>,
    #[serde(default)]
    cleanup_deletion: Option<StorePath>,
    #[serde(default)]
    stage_deletion: Option<StorePath>,
    #[serde(default)]
    publication_quarantine: Option<StorePath>,
    #[serde(default = "default_identity_memory_limit")]
    identity_memory_limit: u64,
    #[serde(default = "default_identity_timeout_millis")]
    identity_timeout_millis: u64,
}

#[derive(Deserialize, Serialize)]
struct Envelope {
    checksum: Box<str>,
    payload: Box<str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageAction {
    ReadSnapshot,
    ReadJournal,
    AppendJournal,
    SyncJournal,
    WriteSnapshotTemporary,
    SyncSnapshotTemporary,
    PublishSnapshot,
    SyncParent,
    ResetJournal,
    Quarantine,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorruptSource {
    Snapshot,
    Journal,
}

pub trait JournalStorage {
    fn read_snapshot(&mut self) -> io::Result<Vec<u8>>;
    fn read_journal(&mut self) -> io::Result<Vec<u8>>;
    fn append_journal(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn sync_journal(&mut self) -> io::Result<()>;
    fn write_snapshot_temporary(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn sync_snapshot_temporary(&mut self) -> io::Result<()>;
    fn publish_snapshot(&mut self) -> io::Result<()>;
    fn sync_parent(&mut self) -> io::Result<()>;
    fn reset_journal(&mut self) -> io::Result<()>;
    /// Saves the corrupt suffix and atomically restores the valid prefix.
    fn quarantine(
        &mut self,
        source: CorruptSource,
        valid_prefix: &[u8],
        corrupt_suffix: &[u8],
    ) -> io::Result<()>;
}

pub struct Journal<S> {
    storage: S,
    records: Vec<JournalRecord>,
    next_sequence: Option<u64>,
    quarantined_records: usize,
}

impl<S: JournalStorage> Journal<S> {
    pub fn open(mut storage: S) -> Result<Self, JournalError> {
        let snapshot = storage.read_snapshot().map_err(JournalError::Storage)?;
        let mut records = Vec::new();
        let mut plans = std::collections::BTreeMap::new();
        let mut quarantined_records =
            append_snapshot_prefix(&snapshot, &mut records, &mut plans, &mut storage)?;

        let journal = storage.read_journal().map_err(JournalError::Storage)?;
        quarantined_records +=
            append_journal_prefix(&journal, &mut records, &mut plans, &mut storage)?;

        let next_sequence = match records.last() {
            Some(record) => record.sequence.checked_add(1),
            None => Some(1),
        };
        Ok(Self {
            storage,
            records,
            next_sequence,
            quarantined_records,
        })
    }

    #[must_use]
    pub const fn quarantined_records(&self) -> usize {
        self.quarantined_records
    }

    pub fn append(
        &mut self,
        job_id: JobId,
        generation: EventGeneration,
        phase: JournalPhase,
        durability: Durability,
    ) -> Result<JournalRecord, JournalError> {
        self.append_record(job_id, generation, phase, durability, None)
    }

    pub fn append_archive(
        &mut self,
        job_id: JobId,
        generation: EventGeneration,
        phase: JournalPhase,
        durability: Durability,
        mut checkpoint: ArchiveCheckpoint,
    ) -> Result<JournalRecord, JournalError> {
        if let Some((shared, digest)) = self.records.iter().rev().find_map(|record| {
            (record.job_id == job_id && record.generation == generation)
                .then_some(record.archive.as_ref())
                .flatten()
                .map(|checkpoint| (checkpoint.shared_plan(), checkpoint.plan_digest))
        }) {
            if checkpoint.plan_digest != digest {
                return Err(JournalError::ArchivePlanChanged);
            }
            checkpoint.plan = shared;
            checkpoint.plan_digest = digest;
        }
        self.append_record(job_id, generation, phase, durability, Some(checkpoint))
    }

    fn append_record(
        &mut self,
        job_id: JobId,
        generation: EventGeneration,
        phase: JournalPhase,
        durability: Durability,
        archive: Option<ArchiveCheckpoint>,
    ) -> Result<JournalRecord, JournalError> {
        let sequence = self.next_sequence.ok_or(JournalError::SequenceExhausted)?;
        let record = JournalRecord {
            schema_version: JOURNAL_SCHEMA_VERSION,
            sequence,
            job_id,
            generation,
            phase,
            durability,
            archive,
        };
        let include_plan = !self.records.iter().any(|existing| {
            existing.job_id == job_id
                && existing.generation == generation
                && existing.archive.is_some()
        });
        let encoded = encode_record(&record, include_plan)?;
        self.storage
            .append_journal(&encoded)
            .map_err(JournalError::Storage)?;
        self.storage.sync_journal().map_err(JournalError::Storage)?;
        self.storage.sync_parent().map_err(JournalError::Storage)?;
        self.records.push(record.clone());
        self.next_sequence = sequence.checked_add(1);
        Ok(record)
    }

    pub fn compact(&mut self) -> Result<(), JournalError> {
        let mut snapshot = Vec::new();
        let mut encoded_plans = std::collections::BTreeSet::new();
        for record in &self.records {
            let key = (record.job_id, record.generation);
            let include_plan = record.archive.is_some() && encoded_plans.insert(key);
            snapshot.extend_from_slice(&encode_record(record, include_plan)?);
        }
        self.storage
            .write_snapshot_temporary(&snapshot)
            .map_err(JournalError::Storage)?;
        self.storage
            .sync_snapshot_temporary()
            .map_err(JournalError::Storage)?;
        self.storage
            .publish_snapshot()
            .map_err(JournalError::Storage)?;
        self.storage.sync_parent().map_err(JournalError::Storage)?;
        self.storage
            .reset_journal()
            .map_err(JournalError::Storage)?;
        self.storage.sync_journal().map_err(JournalError::Storage)
    }

    #[must_use]
    pub const fn storage(&self) -> &S {
        &self.storage
    }

    #[must_use]
    pub fn records(&self) -> &[JournalRecord] {
        &self.records
    }

    #[must_use]
    pub fn into_storage(self) -> S {
        self.storage
    }
}

fn append_snapshot_prefix<S: JournalStorage>(
    bytes: &[u8],
    records: &mut Vec<JournalRecord>,
    plans: &mut std::collections::BTreeMap<[u8; 32], Arc<ArchiveOperationPlan>>,
    storage: &mut S,
) -> Result<usize, JournalError> {
    let decoded = decode_lines(bytes, plans)?;
    let mut corrupt_at = decoded.corrupt_at;
    for line in decoded.lines {
        let expected = u64::try_from(records.len())
            .ok()
            .and_then(|length| length.checked_add(1))
            .ok_or(JournalError::SequenceExhausted)?;
        if line.record.sequence != expected {
            corrupt_at = Some(line.offset);
            break;
        }
        records.push(line.record);
    }
    quarantine_suffix(storage, CorruptSource::Snapshot, bytes, corrupt_at)
}

fn append_journal_prefix<S: JournalStorage>(
    bytes: &[u8],
    records: &mut Vec<JournalRecord>,
    plans: &mut std::collections::BTreeMap<[u8; 32], Arc<ArchiveOperationPlan>>,
    storage: &mut S,
) -> Result<usize, JournalError> {
    let decoded = decode_lines(bytes, plans)?;
    let mut corrupt_at = decoded.corrupt_at;
    for line in decoded.lines {
        let sequence_index = line
            .record
            .sequence
            .checked_sub(1)
            .and_then(|sequence| usize::try_from(sequence).ok());
        match sequence_index.and_then(|index| records.get(index)) {
            Some(snapshot_record) if snapshot_record == &line.record => continue,
            Some(_) => {
                corrupt_at = Some(line.offset);
                break;
            }
            None => {
                let expected = u64::try_from(records.len())
                    .ok()
                    .and_then(|length| length.checked_add(1))
                    .ok_or(JournalError::SequenceExhausted)?;
                if line.record.sequence != expected {
                    corrupt_at = Some(line.offset);
                    break;
                }
                records.push(line.record);
            }
        }
    }
    quarantine_suffix(storage, CorruptSource::Journal, bytes, corrupt_at)
}

fn quarantine_suffix<S: JournalStorage>(
    storage: &mut S,
    source: CorruptSource,
    bytes: &[u8],
    corrupt_at: Option<usize>,
) -> Result<usize, JournalError> {
    let Some(corrupt_at) = corrupt_at else {
        return Ok(0);
    };
    storage
        .quarantine(source, &bytes[..corrupt_at], &bytes[corrupt_at..])
        .map_err(JournalError::Storage)?;
    Ok(bytes[corrupt_at..]
        .split(|byte| *byte == b'\n')
        .filter(|record| !record.is_empty())
        .count()
        .max(1))
}

struct DecodedLine {
    record: JournalRecord,
    offset: usize,
}

struct DecodedLines {
    lines: Vec<DecodedLine>,
    corrupt_at: Option<usize>,
}

fn decode_lines(
    bytes: &[u8],
    plans: &mut std::collections::BTreeMap<[u8; 32], Arc<ArchiveOperationPlan>>,
) -> Result<DecodedLines, JournalError> {
    let mut lines = Vec::new();
    let mut offset = 0;
    for encoded in bytes.split_inclusive(|byte| *byte == b'\n') {
        let line = encoded.strip_suffix(b"\n").unwrap_or(encoded);
        if line.is_empty() {
            offset += encoded.len();
            continue;
        }
        match decode_record(line, plans) {
            Ok(record) => lines.push(DecodedLine { record, offset }),
            Err(
                error @ (JournalError::UnsupportedSchema(_) | JournalError::UnrecognizedSchema),
            ) => {
                return Err(error);
            }
            Err(_) => {
                return Ok(DecodedLines {
                    lines,
                    corrupt_at: Some(offset),
                });
            }
        }
        offset += encoded.len();
    }
    Ok(DecodedLines {
        lines,
        corrupt_at: None,
    })
}

fn encode_record(record: &JournalRecord, include_plan: bool) -> Result<Vec<u8>, JournalError> {
    let archive = record
        .archive
        .as_ref()
        .map(|checkpoint| {
            Ok(ArchiveCheckpointDocument {
                plan: include_plan.then(|| checkpoint.plan().clone()),
                plan_digest: checkpoint.plan_digest,
                staging: checkpoint.staging.clone(),
                staging_identity: checkpoint.staging_identity,
                destination_before: checkpoint.destination_before,
                destination_after: checkpoint.destination_after,
                staging_nonce: checkpoint.staging_nonce,
                cleanup: checkpoint.cleanup.clone(),
                cleanup_kind: checkpoint.cleanup_kind,
                cleanup_identity: checkpoint.cleanup_identity,
                cleanup_deletion: checkpoint.cleanup_deletion.clone(),
                stage_deletion: checkpoint.stage_deletion.clone(),
                publication_quarantine: checkpoint.publication_quarantine.clone(),
                identity_memory_limit: checkpoint.identity_memory_limit,
                identity_timeout_millis: checkpoint.identity_timeout_millis,
            })
        })
        .transpose()?;
    let document = RecordDocument {
        schema_version: record.schema_version,
        sequence: record.sequence,
        job_id: record.job_id.get(),
        generation: record.generation.get(),
        phase: record.phase,
        durability: record.durability.clone(),
        archive,
    };
    let payload = serde_json::to_string(&document).map_err(JournalError::Encode)?;
    let envelope = Envelope {
        checksum: blake3::hash(payload.as_bytes()).to_hex().to_string().into(),
        payload: payload.into(),
    };
    let mut encoded = serde_json::to_vec(&envelope).map_err(JournalError::Encode)?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn decode_record(
    line: &[u8],
    plans: &mut std::collections::BTreeMap<[u8; 32], Arc<ArchiveOperationPlan>>,
) -> Result<JournalRecord, JournalError> {
    let envelope: Envelope = serde_json::from_slice(line).map_err(JournalError::Decode)?;
    let expected = blake3::hash(envelope.payload.as_bytes()).to_hex();
    if envelope.checksum.as_ref() != expected.as_str() {
        return Err(JournalError::ChecksumMismatch);
    }
    let document: RecordDocument = serde_json::from_str(&envelope.payload).map_err(|error| {
        let version = serde_json::from_str::<serde_json::Value>(&envelope.payload)
            .ok()
            .and_then(|value| value.get("schema_version").cloned());
        match version {
            Some(version) => match version.as_u64() {
                Some(version) if version != u64::from(JOURNAL_SCHEMA_VERSION) => {
                    JournalError::UnsupportedSchema(version)
                }
                Some(_) => JournalError::Decode(error),
                None => JournalError::UnrecognizedSchema,
            },
            _ => JournalError::Decode(error),
        }
    })?;
    if document.schema_version != JOURNAL_SCHEMA_VERSION {
        return Err(JournalError::UnsupportedSchema(u64::from(
            document.schema_version,
        )));
    }
    let job_id = JobId::new(document.job_id).ok_or(JournalError::InvalidJobId)?;
    let archive = document
        .archive
        .map(|checkpoint| {
            let digest = if checkpoint.plan_digest == [0; 32] {
                checkpoint
                    .plan
                    .as_ref()
                    .map(|plan| {
                        serde_json::to_vec(plan).map(|bytes| *blake3::hash(&bytes).as_bytes())
                    })
                    .transpose()
                    .map_err(JournalError::Encode)?
                    .ok_or(JournalError::MissingArchivePlan)?
            } else {
                checkpoint.plan_digest
            };
            let plan = if let Some(plan) = checkpoint.plan {
                let plan = Arc::new(plan);
                plans.insert(digest, Arc::clone(&plan));
                plan
            } else {
                plans
                    .get(&digest)
                    .cloned()
                    .ok_or(JournalError::MissingArchivePlan)?
            };
            Ok(ArchiveCheckpoint {
                plan,
                plan_digest: digest,
                staging: checkpoint.staging,
                staging_identity: checkpoint.staging_identity,
                destination_before: checkpoint.destination_before,
                destination_after: checkpoint.destination_after,
                staging_nonce: checkpoint.staging_nonce,
                cleanup: checkpoint.cleanup,
                cleanup_kind: checkpoint.cleanup_kind,
                cleanup_identity: checkpoint.cleanup_identity,
                cleanup_deletion: checkpoint.cleanup_deletion,
                stage_deletion: checkpoint.stage_deletion,
                publication_quarantine: checkpoint.publication_quarantine,
                identity_memory_limit: checkpoint.identity_memory_limit,
                identity_timeout_millis: checkpoint.identity_timeout_millis,
            })
        })
        .transpose()?;
    Ok(JournalRecord {
        schema_version: document.schema_version,
        sequence: document.sequence,
        job_id,
        generation: EventGeneration::new(document.generation),
        phase: document.phase,
        durability: document.durability,
        archive,
    })
}

#[derive(Debug)]
pub enum JournalError {
    Storage(io::Error),
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    ChecksumMismatch,
    UnsupportedSchema(u64),
    UnrecognizedSchema,
    InvalidJobId,
    SequenceExhausted,
    MissingArchivePlan,
    ArchivePlanChanged,
}

impl fmt::Display for JournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => write!(formatter, "journal storage failed: {error}"),
            Self::Encode(error) => write!(formatter, "journal encoding failed: {error}"),
            Self::Decode(error) => write!(formatter, "journal decoding failed: {error}"),
            Self::ChecksumMismatch => formatter.write_str("journal checksum does not match"),
            Self::UnsupportedSchema(version) => {
                write!(formatter, "journal schema {version} is unsupported")
            }
            Self::UnrecognizedSchema => formatter.write_str("journal schema is unrecognized"),
            Self::InvalidJobId => formatter.write_str("journal contains an invalid job ID"),
            Self::SequenceExhausted => formatter.write_str("journal sequence is exhausted"),
            Self::MissingArchivePlan => {
                formatter.write_str("journal archive checkpoint references a missing plan")
            }
            Self::ArchivePlanChanged => {
                formatter.write_str("journal archive plan changed within one job generation")
            }
        }
    }
}

impl Error for JournalError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        let source: &(dyn Error + 'static) = match self {
            Self::Storage(error) => error,
            Self::Encode(error) | Self::Decode(error) => error,
            Self::ChecksumMismatch
            | Self::UnsupportedSchema(_)
            | Self::UnrecognizedSchema
            | Self::InvalidJobId
            | Self::SequenceExhausted
            | Self::MissingArchivePlan
            | Self::ArchivePlanChanged => return None,
        };
        Some(source)
    }
}
