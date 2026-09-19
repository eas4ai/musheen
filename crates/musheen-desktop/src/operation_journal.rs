use musheen_ops::{CorruptSource, JournalStorage};
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

/// Linux filesystem persistence for the operation journal.
pub struct FileJournalStorage {
    directory: PathBuf,
    snapshot_path: PathBuf,
    snapshot_temporary_path: PathBuf,
    journal_path: PathBuf,
    quarantine_directory: PathBuf,
}

impl FileJournalStorage {
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

fn append_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = open_private_append(path)?;
    file.write_all(bytes)
}

fn open_private_append(path: &Path) -> io::Result<File> {
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

fn create_private_directory(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}
