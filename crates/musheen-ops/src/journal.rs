use crate::{EventGeneration, JobId};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::io;

pub const JOURNAL_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalPhase {
    Planned,
    StagingCreated,
    DataCopied,
    MetadataApplied,
    DestinationPublished,
    SourceRemoved,
    StagingCleaned,
    Completed,
    RolledBack,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "level", content = "reason")]
pub enum Durability {
    CrashDurable,
    BestEffort(Box<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalRecord {
    schema_version: u32,
    sequence: u64,
    job_id: JobId,
    generation: EventGeneration,
    phase: JournalPhase,
    durability: Durability,
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
}

#[derive(Deserialize, Serialize)]
struct RecordDocument {
    schema_version: u32,
    sequence: u64,
    job_id: u64,
    generation: u64,
    phase: JournalPhase,
    durability: Durability,
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
        let mut quarantined_records =
            append_snapshot_prefix(&snapshot, &mut records, &mut storage)?;

        let journal = storage.read_journal().map_err(JournalError::Storage)?;
        quarantined_records += append_journal_prefix(&journal, &mut records, &mut storage)?;

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
        let sequence = self.next_sequence.ok_or(JournalError::SequenceExhausted)?;
        let record = JournalRecord {
            schema_version: JOURNAL_SCHEMA_VERSION,
            sequence,
            job_id,
            generation,
            phase,
            durability,
        };
        let encoded = encode_record(&record)?;
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
        for record in &self.records {
            snapshot.extend_from_slice(&encode_record(record)?);
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
    storage: &mut S,
) -> Result<usize, JournalError> {
    let decoded = decode_lines(bytes);
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
    storage: &mut S,
) -> Result<usize, JournalError> {
    let decoded = decode_lines(bytes);
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

fn decode_lines(bytes: &[u8]) -> DecodedLines {
    let mut lines = Vec::new();
    let mut offset = 0;
    for encoded in bytes.split_inclusive(|byte| *byte == b'\n') {
        let line = encoded.strip_suffix(b"\n").unwrap_or(encoded);
        if line.is_empty() {
            offset += encoded.len();
            continue;
        }
        match decode_record(line) {
            Ok(record) => lines.push(DecodedLine { record, offset }),
            Err(_) => {
                return DecodedLines {
                    lines,
                    corrupt_at: Some(offset),
                };
            }
        }
        offset += encoded.len();
    }
    DecodedLines {
        lines,
        corrupt_at: None,
    }
}

fn encode_record(record: &JournalRecord) -> Result<Vec<u8>, JournalError> {
    let document = RecordDocument {
        schema_version: record.schema_version,
        sequence: record.sequence,
        job_id: record.job_id.get(),
        generation: record.generation.get(),
        phase: record.phase,
        durability: record.durability.clone(),
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

fn decode_record(line: &[u8]) -> Result<JournalRecord, JournalError> {
    let envelope: Envelope = serde_json::from_slice(line).map_err(JournalError::Decode)?;
    let expected = blake3::hash(envelope.payload.as_bytes()).to_hex();
    if envelope.checksum.as_ref() != expected.as_str() {
        return Err(JournalError::ChecksumMismatch);
    }
    let document: RecordDocument =
        serde_json::from_str(&envelope.payload).map_err(JournalError::Decode)?;
    if document.schema_version != JOURNAL_SCHEMA_VERSION {
        return Err(JournalError::UnsupportedSchema(document.schema_version));
    }
    let job_id = JobId::new(document.job_id).ok_or(JournalError::InvalidJobId)?;
    Ok(JournalRecord {
        schema_version: document.schema_version,
        sequence: document.sequence,
        job_id,
        generation: EventGeneration::new(document.generation),
        phase: document.phase,
        durability: document.durability,
    })
}

#[derive(Debug)]
pub enum JournalError {
    Storage(io::Error),
    Encode(serde_json::Error),
    Decode(serde_json::Error),
    ChecksumMismatch,
    UnsupportedSchema(u32),
    InvalidJobId,
    SequenceExhausted,
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
            Self::InvalidJobId => formatter.write_str("journal contains an invalid job ID"),
            Self::SequenceExhausted => formatter.write_str("journal sequence is exhausted"),
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
            | Self::InvalidJobId
            | Self::SequenceExhausted => return None,
        };
        Some(source)
    }
}
