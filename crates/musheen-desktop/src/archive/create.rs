use super::budget::{
    ArchiveBudget, ArchiveMemoryLease, ArchiveOperationError, ArchiveOperationLimits, map_io,
};
use super::{ArchivePassword, ArchivePasswordProvider, PasswordRequest};
use musheen_core::CancellationToken;
use musheen_ops::{
    ArchiveCodec, ArchiveConflictPolicy, ArchiveOperationPlan, Durability, EventGeneration, JobId,
    Journal, JournalPhase, JournalStorage, OperationKind, StagingPath,
};
use nix::libc::O_NOFOLLOW;
use sevenz_rust2::encoder_options::AesEncoderOptions;
use sevenz_rust2::{
    ArchiveEntry as SevenEntry, ArchiveWriter as SevenWriter, EncoderMethod, Password,
};
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use zip::write::SimpleFileOptions;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveOperationOutcome {
    Published,
    Skipped,
}

pub fn execute_archive_operation<S: JournalStorage>(
    plan: &ArchiveOperationPlan,
    limits: &ArchiveOperationLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
    journal: &mut Journal<S>,
    job_id: JobId,
    generation: EventGeneration,
) -> Result<ArchiveOperationOutcome, ArchiveOperationError> {
    match plan.kind() {
        OperationKind::Compress => run_archive_creation(
            plan,
            limits,
            passwords,
            cancellation,
            journal,
            job_id,
            generation,
        ),
        OperationKind::Extract => super::extract::execute_extract(
            plan,
            limits,
            passwords,
            cancellation,
            journal,
            job_id,
            generation,
        ),
        _ => Err(ArchiveOperationError::InvalidArchive),
    }
}

struct CreateEntry {
    source: PathBuf,
    archive_name: Vec<u8>,
    kind: CreateEntryKind,
    size: u64,
    device: u64,
    inode: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    _memory: ArchiveMemoryLease,
}

#[derive(Clone, Copy)]
enum CreateEntryKind {
    Directory,
    File,
}

fn run_archive_creation<S: JournalStorage>(
    plan: &ArchiveOperationPlan,
    limits: &ArchiveOperationLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
    journal: &mut Journal<S>,
    job_id: JobId,
    generation: EventGeneration,
) -> Result<ArchiveOperationOutcome, ArchiveOperationError> {
    cancellation.check()?;
    let destination = local_path(plan.destination())?;
    if let Some(outcome) = existing_destination_outcome(&destination, plan.conflict_policy())? {
        return Ok(outcome);
    }
    let mut preflight_limits = limits.clone();
    preflight_limits.max_memory_bytes /= 2;
    let mut budget = ArchiveBudget::new(preflight_limits);
    let entries = collect_create_entries(plan, &mut budget, cancellation)?;
    append_phase(journal, job_id, generation, JournalPhase::Planned)?;

    let staging = staging_path(plan, job_id, generation)?;
    let mut published = false;
    let result = (|| {
        let stage_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
            .map_err(|error| map_io(&error))?;
        append_phase(journal, job_id, generation, JournalPhase::StagingCreated)?;
        write_archive(
            stage_file,
            &entries,
            plan.codec(),
            plan.encrypted(),
            passwords,
            cancellation,
        )?;
        sync_file(&staging)?;
        append_phase(journal, job_id, generation, JournalPhase::DataCopied)?;
        append_phase(journal, job_id, generation, JournalPhase::MetadataApplied)?;
        let outcome = publish_staging(&staging, &destination, plan.conflict_policy())?;
        if outcome == ArchiveOperationOutcome::Skipped {
            remove_owned(&staging)?;
            append_phase(journal, job_id, generation, JournalPhase::RolledBack)?;
            return Ok(outcome);
        }
        published = true;
        sync_parent(&destination)?;
        append_phase(
            journal,
            job_id,
            generation,
            JournalPhase::DestinationPublished,
        )?;
        remove_owned(&staging)?;
        append_phase(journal, job_id, generation, JournalPhase::StagingCleaned)?;
        append_phase(journal, job_id, generation, JournalPhase::Completed)?;
        Ok(ArchiveOperationOutcome::Published)
    })();

    if result.is_err() && !published && staging.exists() {
        let _ = remove_owned(&staging);
        let _ = append_phase(journal, job_id, generation, JournalPhase::RolledBack);
    }
    result
}

fn collect_create_entries(
    plan: &ArchiveOperationPlan,
    budget: &mut ArchiveBudget,
    cancellation: &CancellationToken,
) -> Result<Vec<CreateEntry>, ArchiveOperationError> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let mut total_bytes = 0_u64;
    for source in plan.sources() {
        let source = local_path(source)?;
        let root_name = source.file_name().filter(|name| !name.is_empty()).ok_or(
            ArchiveOperationError::UnsafePath("archive sources need a file name"),
        )?;
        for walked in walkdir::WalkDir::new(&source).follow_links(false) {
            cancellation.check()?;
            let walked = walked.map_err(|_| ArchiveOperationError::Io)?;
            let metadata =
                std::fs::symlink_metadata(walked.path()).map_err(|error| map_io(&error))?;
            let kind = if metadata.file_type().is_dir() {
                CreateEntryKind::Directory
            } else if metadata.file_type().is_file() {
                CreateEntryKind::File
            } else {
                return Err(ArchiveOperationError::UnsupportedFileType);
            };
            let relative = walked
                .path()
                .strip_prefix(&source)
                .map_err(|_| ArchiveOperationError::UnsafePath("source escaped its root"))?;
            let mut archive_name = root_name.as_bytes().to_vec();
            if !relative.as_os_str().is_empty() {
                archive_name.push(b'/');
                archive_name.extend_from_slice(relative.as_os_str().as_bytes());
            }
            budget.check_path(&archive_name)?;
            let archive_name =
                super::ArchivePath::normalize_bytes(&archive_name, super::ArchivePath::MAX_BYTES)?;
            if !seen.insert(archive_name.clone()) {
                return Err(ArchiveOperationError::InvalidArchive);
            }
            budget.charge_entry()?;
            let size = if matches!(kind, CreateEntryKind::File) {
                metadata.len()
            } else {
                0
            };
            total_bytes =
                total_bytes
                    .checked_add(size)
                    .ok_or(ArchiveOperationError::LimitExceeded {
                        resource: "expanded bytes",
                        value: u64::MAX,
                        maximum: u64::MAX,
                    })?;
            let name_bytes = u64::try_from(archive_name.len()).unwrap_or(u64::MAX);
            let entry_temporary = size
                .saturating_add(name_bytes.saturating_mul(2))
                .saturating_add(4 * 1_024);
            budget.charge_temporary(entry_temporary)?;
            let memory = budget.reserve_memory(
                u64::try_from(archive_name.len().saturating_add(128)).unwrap_or(u64::MAX),
            )?;
            entries.push(CreateEntry {
                source: walked.path().to_path_buf(),
                archive_name,
                kind,
                size,
                device: metadata.dev(),
                inode: metadata.ino(),
                modified_seconds: metadata.mtime(),
                modified_nanoseconds: metadata.mtime_nsec(),
                _memory: memory,
            });
        }
    }
    budget.charge_expanded(total_bytes, total_bytes.max(1))?;
    budget.charge_temporary(1_024 * 1_024)?;
    Ok(entries)
}

fn write_archive(
    stage: File,
    entries: &[CreateEntry],
    codec: ArchiveCodec,
    encrypted: bool,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
) -> Result<(), ArchiveOperationError> {
    match codec {
        ArchiveCodec::Zip => write_zip(stage, entries, encrypted, passwords, cancellation),
        ArchiveCodec::Tar => write_tar(stage, entries, TarEncoder::Plain, cancellation),
        ArchiveCodec::TarGzip => write_tar(stage, entries, TarEncoder::Gzip, cancellation),
        ArchiveCodec::TarZstd => write_tar(stage, entries, TarEncoder::Zstd, cancellation),
        ArchiveCodec::SevenZip => {
            write_seven_zip(stage, entries, encrypted, passwords, cancellation)
        }
    }
}

fn requested_password(
    codec: ArchiveCodec,
    passwords: &dyn ArchivePasswordProvider,
) -> Result<ArchivePassword, ArchiveOperationError> {
    let format = match codec {
        ArchiveCodec::Zip => super::ArchiveFormat::Zip,
        ArchiveCodec::SevenZip => super::ArchiveFormat::SevenZip,
        _ => return Err(ArchiveOperationError::InvalidArchive),
    };
    passwords
        .request_password(&PasswordRequest { format })?
        .ok_or(ArchiveOperationError::PasswordRequired)
}

fn write_zip(
    stage: File,
    entries: &[CreateEntry],
    encrypted: bool,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
) -> Result<(), ArchiveOperationError> {
    let password = encrypted
        .then(|| requested_password(ArchiveCodec::Zip, passwords))
        .transpose()?;
    let password = password
        .as_ref()
        .map(|value| {
            std::str::from_utf8(value.as_bytes())
                .map_err(|_| ArchiveOperationError::UnsupportedName)
        })
        .transpose()?;
    let mut writer = zip::ZipWriter::new(stage);
    for entry in entries {
        cancellation.check()?;
        verify_entry(entry)?;
        let name = std::str::from_utf8(&entry.archive_name)
            .map_err(|_| ArchiveOperationError::UnsupportedName)?;
        let mut options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(if matches!(entry.kind, CreateEntryKind::Directory) {
                0o755
            } else {
                0o644
            });
        if let Some(password) = password {
            options = options.with_aes_encryption(zip::AesMode::Aes256, password);
        }
        match entry.kind {
            CreateEntryKind::Directory => writer
                .add_directory(format!("{name}/"), options)
                .map_err(map_zip_error)?,
            CreateEntryKind::File => {
                writer.start_file(name, options).map_err(map_zip_error)?;
                let file = open_verified_file(entry)?;
                io::copy(&mut CancellableReader::new(file, cancellation), &mut writer)
                    .map_err(|error| map_io(&error))?;
                verify_entry(entry)?;
            }
        }
    }
    let file = writer.finish().map_err(map_zip_error)?;
    file.sync_all().map_err(|error| map_io(&error))
}

enum TarEncoder {
    Plain,
    Gzip,
    Zstd,
}

fn write_tar(
    stage: File,
    entries: &[CreateEntry],
    encoder: TarEncoder,
    cancellation: &CancellationToken,
) -> Result<(), ArchiveOperationError> {
    match encoder {
        TarEncoder::Plain => write_tar_stream(stage, entries, cancellation)?
            .sync_all()
            .map_err(|error| map_io(&error)),
        TarEncoder::Gzip => {
            let encoder = flate2::write::GzEncoder::new(stage, flate2::Compression::default());
            let encoder = write_tar_stream(encoder, entries, cancellation)?;
            encoder
                .finish()
                .map_err(|error| map_io(&error))?
                .sync_all()
                .map_err(|error| map_io(&error))
        }
        TarEncoder::Zstd => {
            let encoder =
                zstd::stream::write::Encoder::new(stage, 0).map_err(|error| map_io(&error))?;
            let encoder = write_tar_stream(encoder, entries, cancellation)?;
            encoder
                .finish()
                .map_err(|error| map_io(&error))?
                .sync_all()
                .map_err(|error| map_io(&error))
        }
    }
}

fn write_tar_stream<W: Write>(
    writer: W,
    entries: &[CreateEntry],
    cancellation: &CancellationToken,
) -> Result<W, ArchiveOperationError> {
    let mut builder = tar::Builder::new(writer);
    for entry in entries {
        cancellation.check()?;
        verify_entry(entry)?;
        let name = PathBuf::from(OsString::from_vec(entry.archive_name.clone()));
        let mut header = tar::Header::new_gnu();
        header.set_mode(if matches!(entry.kind, CreateEntryKind::Directory) {
            0o755
        } else {
            0o644
        });
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        match entry.kind {
            CreateEntryKind::Directory => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_cksum();
                builder
                    .append_data(&mut header, name, io::empty())
                    .map_err(|error| map_io(&error))?;
            }
            CreateEntryKind::File => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(entry.size);
                header.set_cksum();
                let file = open_verified_file(entry)?;
                builder
                    .append_data(
                        &mut header,
                        name,
                        CancellableReader::new(file, cancellation),
                    )
                    .map_err(|error| map_io(&error))?;
                verify_entry(entry)?;
            }
        }
    }
    builder.finish().map_err(|error| map_io(&error))?;
    builder.into_inner().map_err(|error| map_io(&error))
}

fn write_seven_zip(
    stage: File,
    entries: &[CreateEntry],
    encrypted: bool,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
) -> Result<(), ArchiveOperationError> {
    let password = encrypted
        .then(|| requested_password(ArchiveCodec::SevenZip, passwords))
        .transpose()?;
    let mut writer = SevenWriter::new(stage).map_err(map_seven_create_error)?;
    if let Some(password) = password.as_ref() {
        let password = std::str::from_utf8(password.as_bytes())
            .map_err(|_| ArchiveOperationError::InvalidPassword)?;
        writer.set_content_methods(vec![
            AesEncoderOptions::new(Password::new(password)).into(),
            EncoderMethod::LZMA2.into(),
        ]);
    }
    for entry in entries {
        cancellation.check()?;
        verify_entry(entry)?;
        let name = std::str::from_utf8(&entry.archive_name)
            .map_err(|_| ArchiveOperationError::UnsupportedName)?;
        match entry.kind {
            CreateEntryKind::Directory => {
                writer
                    .push_archive_entry::<&[u8]>(SevenEntry::new_directory(name), None)
                    .map_err(map_seven_create_error)?;
            }
            CreateEntryKind::File => {
                let file = open_verified_file(entry)?;
                writer
                    .push_archive_entry(
                        SevenEntry::new_file(name),
                        Some(CancellableReader::new(file, cancellation)),
                    )
                    .map_err(|error| {
                        if cancellation.is_cancelled() {
                            ArchiveOperationError::Cancelled
                        } else {
                            map_seven_create_error(error)
                        }
                    })?;
                verify_entry(entry)?;
            }
        }
    }
    writer
        .finish()
        .map_err(|error| map_io(&error))?
        .sync_all()
        .map_err(|error| map_io(&error))
}

fn map_zip_error(error: zip::result::ZipError) -> ArchiveOperationError {
    match error {
        zip::result::ZipError::Io(error) => map_io(&error),
        _ => ArchiveOperationError::Io,
    }
}

fn map_seven_create_error(error: sevenz_rust2::Error) -> ArchiveOperationError {
    match error {
        sevenz_rust2::Error::Io(error, _) | sevenz_rust2::Error::FileOpen(error, _) => {
            map_io(&error)
        }
        sevenz_rust2::Error::PasswordRequired => ArchiveOperationError::PasswordRequired,
        sevenz_rust2::Error::MaybeBadPassword(_) => ArchiveOperationError::InvalidPassword,
        _ => ArchiveOperationError::Io,
    }
}

fn verify_entry(entry: &CreateEntry) -> Result<(), ArchiveOperationError> {
    let metadata = std::fs::symlink_metadata(&entry.source).map_err(|error| map_io(&error))?;
    let kind_matches = match entry.kind {
        CreateEntryKind::Directory => metadata.file_type().is_dir(),
        CreateEntryKind::File => metadata.file_type().is_file(),
    };
    let size_matches =
        matches!(entry.kind, CreateEntryKind::Directory) || metadata.len() == entry.size;
    if !kind_matches
        || metadata.dev() != entry.device
        || metadata.ino() != entry.inode
        || !size_matches
        || metadata.mtime() != entry.modified_seconds
        || metadata.mtime_nsec() != entry.modified_nanoseconds
    {
        return Err(ArchiveOperationError::UnsupportedFileType);
    }
    Ok(())
}

fn open_verified_file(entry: &CreateEntry) -> Result<File, ArchiveOperationError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(&entry.source)
        .map_err(|error| map_io(&error))?;
    let metadata = file.metadata().map_err(|error| map_io(&error))?;
    if !metadata.is_file()
        || metadata.dev() != entry.device
        || metadata.ino() != entry.inode
        || metadata.len() != entry.size
        || metadata.mtime() != entry.modified_seconds
        || metadata.mtime_nsec() != entry.modified_nanoseconds
    {
        return Err(ArchiveOperationError::UnsupportedFileType);
    }
    Ok(file)
}

struct CancellableReader<'a> {
    inner: File,
    cancellation: &'a CancellationToken,
}

impl<'a> CancellableReader<'a> {
    fn new(inner: File, cancellation: &'a CancellationToken) -> Self {
        Self {
            inner,
            cancellation,
        }
    }
}

impl Read for CancellableReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancellation.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "archive operation cancelled",
            ));
        }
        self.inner.read(buffer)
    }
}

pub(crate) fn append_phase<S: JournalStorage>(
    journal: &mut Journal<S>,
    job_id: JobId,
    generation: EventGeneration,
    phase: JournalPhase,
) -> Result<(), ArchiveOperationError> {
    journal
        .append(job_id, generation, phase, Durability::CrashDurable)
        .map(|_| ())
        .map_err(|_| ArchiveOperationError::Journal)
}

pub(crate) fn staging_path(
    plan: &ArchiveOperationPlan,
    job_id: JobId,
    generation: EventGeneration,
) -> Result<PathBuf, ArchiveOperationError> {
    StagingPath::for_destination(plan.destination(), job_id, generation)
        .map_err(|_| ArchiveOperationError::UnsafePath("archive staging needs a local parent"))?
        .path()
        .as_unix_path()
        .map(Path::to_path_buf)
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive staging needs a local path",
        ))
}

pub(crate) fn local_path(path: &musheen_core::StorePath) -> Result<PathBuf, ArchiveOperationError> {
    path.as_unix_path()
        .map(Path::to_path_buf)
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive operations require local paths",
        ))
}

fn existing_destination_outcome(
    destination: &Path,
    policy: ArchiveConflictPolicy,
) -> Result<Option<ArchiveOperationOutcome>, ArchiveOperationError> {
    if !destination.exists() {
        return Ok(None);
    }
    match policy {
        ArchiveConflictPolicy::Fail => Err(ArchiveOperationError::Conflict),
        ArchiveConflictPolicy::Skip => Ok(Some(ArchiveOperationOutcome::Skipped)),
        ArchiveConflictPolicy::Replace => Ok(None),
    }
}

pub(crate) fn publish_staging(
    staging: &Path,
    destination: &Path,
    policy: ArchiveConflictPolicy,
) -> Result<ArchiveOperationOutcome, ArchiveOperationError> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    match renameat_with(CWD, staging, CWD, destination, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(ArchiveOperationOutcome::Published),
        Err(error) if error == rustix::io::Errno::EXIST => match policy {
            ArchiveConflictPolicy::Fail => Err(ArchiveOperationError::Conflict),
            ArchiveConflictPolicy::Skip => Ok(ArchiveOperationOutcome::Skipped),
            ArchiveConflictPolicy::Replace => {
                match renameat_with(CWD, staging, CWD, destination, RenameFlags::EXCHANGE) {
                    Ok(()) => Ok(ArchiveOperationOutcome::Published),
                    Err(error) if error == rustix::io::Errno::NOENT => {
                        renameat_with(CWD, staging, CWD, destination, RenameFlags::NOREPLACE)
                            .map_err(|error| {
                                map_io(&io::Error::from_raw_os_error(error.raw_os_error()))
                            })?;
                        Ok(ArchiveOperationOutcome::Published)
                    }
                    Err(error) => Err(map_io(&io::Error::from_raw_os_error(error.raw_os_error()))),
                }
            }
        },
        Err(error) => Err(map_io(&io::Error::from_raw_os_error(error.raw_os_error()))),
    }
}

pub(crate) fn remove_owned(path: &Path) -> Result<(), ArchiveOperationError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(map_io(&error)),
    }
    .map_err(|error| map_io(&error))
}

pub(crate) fn sync_parent(path: &Path) -> Result<(), ArchiveOperationError> {
    let parent = path.parent().ok_or(ArchiveOperationError::UnsafePath(
        "archive destination needs a parent",
    ))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| map_io(&error))
}

fn sync_file(path: &Path) -> Result<(), ArchiveOperationError> {
    OpenOptions::new()
        .read(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| map_io(&error))
}
