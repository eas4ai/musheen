use musheen_ops::{
    BatchRenameJournal, BatchRenameStep, CorruptSource, JournalStorage, MutationError,
};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const SNAPSHOT_FILE: &str = "operations.snapshot";
const SNAPSHOT_TEMP_FILE: &str = "operations.snapshot.tmp";
const JOURNAL_FILE: &str = "operations.journal";
const QUARANTINE_DIRECTORY: &str = "quarantine";
static NEXT_PRIVATE_FILE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchRenameRecovery {
    steps: Vec<BatchRenameStep>,
    completed_steps: Vec<usize>,
}

impl BatchRenameRecovery {
    #[must_use]
    pub fn steps(&self) -> &[BatchRenameStep] {
        &self.steps
    }

    #[must_use]
    pub fn completed_steps(&self) -> &[usize] {
        &self.completed_steps
    }
}

pub struct FileBatchRenameJournal {
    directory: PathBuf,
    path: PathBuf,
}

impl FileBatchRenameJournal {
    pub fn at(directory: impl Into<PathBuf>, operation_id: u64) -> io::Result<Self> {
        let directory = directory.into();
        create_private_directory(&directory)?;
        Ok(Self {
            path: directory.join(format!("batch-rename-{operation_id}.journal")),
            directory,
        })
    }

    pub fn recovery(&self) -> io::Result<Option<BatchRenameRecovery>> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut steps = None;
        let mut completed_steps = Vec::new();
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            match decode_batch_record(line)? {
                BatchRenameRecord::Plan { schema: 1, value } if steps.is_none() => {
                    steps = Some(value);
                }
                BatchRenameRecord::Completed { schema: 1, index } => {
                    completed_steps.push(index);
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid batch rename journal",
                    ));
                }
            }
        }
        let steps = steps.ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "batch rename plan is missing")
        })?;
        if completed_steps
            .iter()
            .enumerate()
            .any(|(expected, actual)| expected != *actual || *actual >= steps.len())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "batch rename completion sequence is invalid",
            ));
        }
        Ok(Some(BatchRenameRecovery {
            steps,
            completed_steps,
        }))
    }

    pub fn finish(self) -> io::Result<()> {
        match fs::remove_file(self.path) {
            Ok(()) => sync_directory(&self.directory),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl BatchRenameJournal for FileBatchRenameJournal {
    fn persist_plan(&mut self, steps: &[BatchRenameStep]) -> Result<(), MutationError> {
        let record = BatchRenameRecord::Plan {
            schema: 1,
            value: steps.to_vec(),
        };
        let bytes = encode_batch_record(&record).map_err(journal_mutation_error)?;
        let temporary = create_unique_private_file(&self.directory, "batch-rename-plan", &bytes)
            .map_err(journal_mutation_error)?;
        File::open(&temporary)
            .and_then(|file| file.sync_all())
            .map_err(journal_mutation_error)?;
        fs::hard_link(&temporary, &self.path).map_err(journal_mutation_error)?;
        fs::remove_file(temporary).map_err(journal_mutation_error)?;
        sync_directory(&self.directory).map_err(journal_mutation_error)
    }

    fn persist_completed_step(&mut self, index: usize) -> Result<(), MutationError> {
        let record = BatchRenameRecord::Completed { schema: 1, index };
        let bytes = encode_batch_record(&record).map_err(journal_mutation_error)?;
        append_private(&self.path, &bytes).map_err(journal_mutation_error)?;
        open_private_append(&self.path)
            .and_then(|file| file.sync_all())
            .map_err(journal_mutation_error)?;
        sync_directory(&self.directory).map_err(journal_mutation_error)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum BatchRenameRecord {
    Plan {
        schema: u32,
        value: Vec<BatchRenameStep>,
    },
    Completed {
        schema: u32,
        index: usize,
    },
}

#[derive(Deserialize, Serialize)]
struct BatchEnvelope {
    checksum: Box<str>,
    payload: Box<str>,
}

fn encode_batch_record(record: &BatchRenameRecord) -> io::Result<Vec<u8>> {
    let payload = serde_json::to_string(record).map_err(io::Error::other)?;
    let envelope = BatchEnvelope {
        checksum: blake3::hash(payload.as_bytes()).to_hex().to_string().into(),
        payload: payload.into(),
    };
    let mut bytes = serde_json::to_vec(&envelope).map_err(io::Error::other)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn decode_batch_record(line: &[u8]) -> io::Result<BatchRenameRecord> {
    let envelope: BatchEnvelope = serde_json::from_slice(line).map_err(io::Error::other)?;
    let actual = blake3::hash(envelope.payload.as_bytes())
        .to_hex()
        .to_string();
    if actual != envelope.checksum.as_ref() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "batch rename journal checksum mismatch",
        ));
    }
    serde_json::from_str(&envelope.payload).map_err(io::Error::other)
}

fn journal_mutation_error(error: impl std::fmt::Display) -> MutationError {
    MutationError::Provider(format!("batch rename journal failed: {error}").into())
}

/// Linux filesystem persistence for the operation journal.
pub struct FileJournalStorage {
    directory: PathBuf,
    snapshot_path: PathBuf,
    snapshot_temporary_path: PathBuf,
    journal_path: PathBuf,
    quarantine_directory: PathBuf,
}

impl FileJournalStorage {
    pub fn for_current_user() -> io::Result<Self> {
        Self::from_config_home(freedesktop::xdg_config_home())
    }

    pub fn from_config_home(config_home: impl AsRef<Path>) -> io::Result<Self> {
        Self::at(config_home.as_ref().join("musheen/archive-operations"))
    }

    pub fn at(directory: impl Into<PathBuf>) -> io::Result<Self> {
        let directory = directory.into();
        create_private_directory(&directory)?;
        Ok(Self {
            snapshot_path: directory.join(SNAPSHOT_FILE),
            snapshot_temporary_path: directory.join(SNAPSHOT_TEMP_FILE),
            journal_path: directory.join(JOURNAL_FILE),
            quarantine_directory: directory.join(QUARANTINE_DIRECTORY),
            directory,
        })
    }

    #[must_use]
    pub fn journal_path(&self) -> &Path {
        &self.journal_path
    }

    fn source_path(&self, source: CorruptSource) -> &Path {
        match source {
            CorruptSource::Snapshot => &self.snapshot_path,
            CorruptSource::Journal => &self.journal_path,
        }
    }
}

impl JournalStorage for FileJournalStorage {
    fn read_snapshot(&mut self) -> io::Result<Vec<u8>> {
        read_if_present(&self.snapshot_path)
    }

    fn read_journal(&mut self) -> io::Result<Vec<u8>> {
        read_if_present(&self.journal_path)
    }

    fn append_journal(&mut self, bytes: &[u8]) -> io::Result<()> {
        append_private(&self.journal_path, bytes)
    }

    fn sync_journal(&mut self) -> io::Result<()> {
        open_private_append(&self.journal_path)?.sync_all()
    }

    fn write_snapshot_temporary(&mut self, bytes: &[u8]) -> io::Result<()> {
        write_private(&self.snapshot_temporary_path, bytes, false)
    }

    fn sync_snapshot_temporary(&mut self) -> io::Result<()> {
        File::open(&self.snapshot_temporary_path)?.sync_all()
    }

    fn publish_snapshot(&mut self) -> io::Result<()> {
        fs::rename(&self.snapshot_temporary_path, &self.snapshot_path)
    }

    fn sync_parent(&mut self) -> io::Result<()> {
        sync_directory(&self.directory)
    }

    fn reset_journal(&mut self) -> io::Result<()> {
        write_private(&self.journal_path, &[], false)
    }

    fn quarantine(
        &mut self,
        source: CorruptSource,
        valid_prefix: &[u8],
        corrupt_suffix: &[u8],
    ) -> io::Result<()> {
        create_private_directory(&self.quarantine_directory)?;
        let label = match source {
            CorruptSource::Snapshot => "snapshot",
            CorruptSource::Journal => "journal",
        };
        let quarantine_path = create_unique_private_file(
            &self.quarantine_directory,
            &format!("{label}.corrupt"),
            corrupt_suffix,
        )?;
        File::open(quarantine_path)?.sync_all()?;
        sync_directory(&self.quarantine_directory)?;

        let source_path = self.source_path(source);
        let replacement_path =
            create_unique_private_file(&self.directory, &format!("{label}.repair"), valid_prefix)?;
        File::open(&replacement_path)?.sync_all()?;
        fs::rename(replacement_path, source_path)?;
        sync_directory(&self.directory)
    }
}

fn read_if_present(path: &Path) -> io::Result<Vec<u8>> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

pub(crate) fn append_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = open_private_append(path)?;
    file.write_all(bytes)
}

pub(crate) fn open_private_append(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

fn write_private(path: &Path, bytes: &[u8], create_new: bool) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create(!create_new)
        .create_new(create_new)
        .write(true)
        .truncate(!create_new)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)
}

fn create_unique_private_file(directory: &Path, stem: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    loop {
        let sequence = NEXT_PRIVATE_FILE.fetch_add(1, Ordering::Relaxed);
        let path = directory.join(format!("{stem}.{sequence}"));
        match write_private(&path, bytes, true) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn create_private_directory(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}
