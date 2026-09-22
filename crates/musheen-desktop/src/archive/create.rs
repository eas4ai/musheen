use super::budget::{
    ArchiveBudget, ArchiveMemoryLease, ArchiveOperationAccounting, ArchiveOperationError,
    ArchiveOperationLimits, map_io,
};
use super::{ArchivePassword, ArchivePasswordProvider, PasswordRequest};
use musheen_core::CancellationToken;
use musheen_ops::{
    ArchiveCheckpoint, ArchiveCodec, ArchiveConflictPolicy, ArchiveEventPhase,
    ArchiveOperationPlan, ArchivePathIdentity, Clock, Durability, EventGeneration, JobId, Journal,
    JournalPhase, JournalStorage, OperationKind, ScheduledJob, Scheduler, StagingPath,
};
use nix::libc::O_NOFOLLOW;
use sevenz_rust2::encoder_options::AesEncoderOptions;
use sevenz_rust2::{
    ArchiveEntry as SevenEntry, ArchiveWriter as SevenWriter, EncoderMethod, Password,
};
use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use zip::write::SimpleFileOptions;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveOperationOutcome {
    Published,
    Skipped,
}

pub fn execute_scheduled_archive_operation<C: Clock, S: JournalStorage>(
    scheduler: &mut Scheduler<C>,
    job: &ScheduledJob,
    limits: &ArchiveOperationLimits,
    passwords: &dyn ArchivePasswordProvider,
    journal: &mut Journal<S>,
) -> Result<ArchiveOperationOutcome, ArchiveOperationError> {
    let accounting = ArchiveOperationAccounting::default();
    execute_scheduled_archive_operation_with_accounting(
        scheduler,
        job,
        limits,
        passwords,
        journal,
        &accounting,
    )
}

pub fn execute_scheduled_archive_operation_with_accounting<C: Clock, S: JournalStorage>(
    scheduler: &mut Scheduler<C>,
    job: &ScheduledJob,
    limits: &ArchiveOperationLimits,
    passwords: &dyn ArchivePasswordProvider,
    journal: &mut Journal<S>,
    accounting: &ArchiveOperationAccounting,
) -> Result<ArchiveOperationOutcome, ArchiveOperationError> {
    let plan = job
        .archive_plan()
        .ok_or(ArchiveOperationError::InvalidArchive)?
        .clone();
    let job_id = job.id();
    let generation = job.generation();
    let cancellation = job.cancellation().clone();
    let mut report_phase = |phase| {
        scheduler
            .emit_archive_phase(job_id, phase)
            .map_err(|_| ArchiveOperationError::Engine)
    };
    let result = execute_archive_operation(
        &plan,
        limits,
        passwords,
        &cancellation,
        journal,
        job_id,
        generation,
        &mut report_phase,
        accounting,
    );
    match &result {
        Ok(_) => scheduler
            .complete(job_id)
            .map_err(|_| ArchiveOperationError::Engine)?,
        Err(ArchiveOperationError::Cancelled) => scheduler
            .finish_cancel(job_id)
            .map_err(|_| ArchiveOperationError::Engine)?,
        Err(_) => scheduler
            .fail(job_id)
            .map_err(|_| ArchiveOperationError::Engine)?,
    }
    result
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_archive_operation<S: JournalStorage>(
    plan: &ArchiveOperationPlan,
    limits: &ArchiveOperationLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
    journal: &mut Journal<S>,
    job_id: JobId,
    generation: EventGeneration,
    report_phase: &mut dyn FnMut(ArchiveEventPhase) -> Result<(), ArchiveOperationError>,
    accounting: &ArchiveOperationAccounting,
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
            report_phase,
            accounting,
        ),
        OperationKind::Extract => super::extract::execute_extract(
            plan,
            limits,
            passwords,
            cancellation,
            journal,
            job_id,
            generation,
            report_phase,
            accounting,
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

#[allow(clippy::too_many_arguments)]
fn run_archive_creation<S: JournalStorage>(
    plan: &ArchiveOperationPlan,
    limits: &ArchiveOperationLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
    journal: &mut Journal<S>,
    job_id: JobId,
    generation: EventGeneration,
    report_phase: &mut dyn FnMut(ArchiveEventPhase) -> Result<(), ArchiveOperationError>,
    accounting: &ArchiveOperationAccounting,
) -> Result<ArchiveOperationOutcome, ArchiveOperationError> {
    report_phase(ArchiveEventPhase::Preflight)?;
    cancellation.check()?;
    let budget = Rc::new(RefCell::new(ArchiveBudget::with_accounting(
        limits.clone(),
        accounting.clone(),
    )));
    let plan_path_bytes = plan
        .sources()
        .iter()
        .chain(std::iter::once(plan.destination()))
        .map(|path| {
            path.as_unix_path()
                .map_or(0, |value| value.as_os_str().len())
        })
        .sum::<usize>();
    let _plan_paths_memory = budget
        .borrow()
        .reserve_memory(u64::try_from(plan_path_bytes).unwrap_or(u64::MAX))?;
    let destination = local_path(plan.destination())?;
    if let Some(outcome) = existing_destination_outcome(&destination, plan.conflict_policy())? {
        return Ok(outcome);
    }
    let entries = collect_create_entries(plan, &mut budget.borrow_mut(), cancellation)?;
    let staging = staging_path(plan, job_id, generation)?;
    let destination_before = path_identity(&destination)?;
    append_archive_phase(
        journal,
        job_id,
        generation,
        JournalPhase::Planned,
        plan,
        &staging,
        destination_before,
        None,
    )?;
    let mut published = false;
    let result = (|| {
        let stage_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staging)
            .map_err(|error| map_io(&error))?;
        sync_parent(&staging)?;
        report_phase(ArchiveEventPhase::Staging)?;
        append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::StagingCreated,
            plan,
            &staging,
            destination_before,
            None,
        )?;
        report_phase(ArchiveEventPhase::Encoding)?;
        write_archive(
            stage_file,
            &entries,
            plan.codec(),
            plan.encrypted(),
            passwords,
            cancellation,
            Rc::clone(&budget),
        )?;
        sync_file(&staging)?;
        append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::DataCopied,
            plan,
            &staging,
            destination_before,
            None,
        )?;
        append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::MetadataApplied,
            plan,
            &staging,
            destination_before,
            None,
        )?;
        report_phase(ArchiveEventPhase::Publishing)?;
        let outcome = publish_staging(&staging, &destination, plan.conflict_policy())?;
        if outcome == ArchiveOperationOutcome::Skipped {
            remove_owned(&staging)?;
            append_archive_phase(
                journal,
                job_id,
                generation,
                JournalPhase::RolledBack,
                plan,
                &staging,
                destination_before,
                None,
            )?;
            return Ok(outcome);
        }
        published = true;
        sync_parent(&destination)?;
        let destination_after = path_identity(&destination)?;
        append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::DestinationPublished,
            plan,
            &staging,
            destination_before,
            destination_after,
        )?;
        report_phase(ArchiveEventPhase::Cleaning)?;
        remove_owned(&staging)?;
        append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::StagingCleaned,
            plan,
            &staging,
            destination_before,
            destination_after,
        )?;
        append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::Completed,
            plan,
            &staging,
            destination_before,
            destination_after,
        )?;
        Ok(ArchiveOperationOutcome::Published)
    })();

    if result.is_err() && !published && staging.exists() {
        let _ = remove_owned(&staging);
        let _ = append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::RolledBack,
            plan,
            &staging,
            destination_before,
            None,
        );
    }
    result
}

fn collect_create_entries(
    plan: &ArchiveOperationPlan,
    budget: &mut ArchiveBudget,
    cancellation: &CancellationToken,
) -> Result<Vec<CreateEntry>, ArchiveOperationError> {
    let mut entries = Vec::new();
    let mut total_bytes = 0_u64;
    let mut projected_temporary = 0_u64;
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
            let relative_bytes = relative.as_os_str().as_bytes();
            let archive_name_len = root_name
                .as_bytes()
                .len()
                .saturating_add((!relative_bytes.is_empty()) as usize)
                .saturating_add(relative_bytes.len());
            budget.check_path_len(archive_name_len)?;
            let allocation_bytes = archive_name_len
                .saturating_mul(2)
                .saturating_add(walked.path().as_os_str().as_bytes().len())
                .saturating_add(std::mem::size_of::<CreateEntry>());
            let memory =
                budget.reserve_memory(u64::try_from(allocation_bytes).unwrap_or(u64::MAX))?;
            entries
                .try_reserve_exact(1)
                .map_err(|_| ArchiveOperationError::Io)?;
            let mut archive_name = Vec::with_capacity(archive_name_len);
            archive_name.extend_from_slice(root_name.as_bytes());
            if !relative_bytes.is_empty() {
                archive_name.push(b'/');
                archive_name.extend_from_slice(relative_bytes);
            }
            let archive_name =
                super::ArchivePath::normalize_bytes(&archive_name, super::ArchivePath::MAX_BYTES)?;
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
            projected_temporary = projected_temporary.saturating_add(entry_temporary);
            budget.check_temporary(projected_temporary)?;
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
    entries.sort_by(|left, right| left.archive_name.cmp(&right.archive_name));
    if entries
        .windows(2)
        .any(|pair| pair[0].archive_name == pair[1].archive_name)
    {
        return Err(ArchiveOperationError::InvalidArchive);
    }
    budget.charge_expanded(total_bytes, total_bytes.max(1))?;
    budget.check_temporary(projected_temporary.saturating_add(1_024 * 1_024))?;
    Ok(entries)
}

fn write_archive(
    stage: File,
    entries: &[CreateEntry],
    codec: ArchiveCodec,
    encrypted: bool,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
    budget: Rc<RefCell<ArchiveBudget>>,
) -> Result<(), ArchiveOperationError> {
    let state = Rc::new(RefCell::new(None));
    let stage = BudgetedWriteSeek::new(stage, budget, Rc::clone(&state));
    let result = match codec {
        ArchiveCodec::Zip => write_zip(stage, entries, encrypted, passwords, cancellation),
        ArchiveCodec::Tar => write_tar(stage, entries, TarEncoder::Plain, cancellation),
        ArchiveCodec::TarGzip => write_tar(stage, entries, TarEncoder::Gzip, cancellation),
        ArchiveCodec::TarZstd => write_tar(stage, entries, TarEncoder::Zstd, cancellation),
        ArchiveCodec::SevenZip => {
            write_seven_zip(stage, entries, encrypted, passwords, cancellation)
        }
    };
    if let Some(error) = state.borrow_mut().take() {
        return Err(error);
    }
    result
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

struct BudgetedWriteSeek {
    inner: File,
    budget: Rc<RefCell<ArchiveBudget>>,
    error: Rc<RefCell<Option<ArchiveOperationError>>>,
    position: u64,
    high_water: u64,
}

impl BudgetedWriteSeek {
    fn new(
        inner: File,
        budget: Rc<RefCell<ArchiveBudget>>,
        error: Rc<RefCell<Option<ArchiveOperationError>>>,
    ) -> Self {
        Self {
            inner,
            budget,
            error,
            position: 0,
            high_water: 0,
        }
    }

    fn sync_all(self) -> Result<(), ArchiveOperationError> {
        self.inner.sync_all().map_err(|error| map_io(&error))
    }

    fn reserve_memory(&self, bytes: u64) -> Result<ArchiveMemoryLease, ArchiveOperationError> {
        self.budget.borrow().reserve_memory(bytes)
    }
}

impl Write for BudgetedWriteSeek {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let requested = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let end = self.position.saturating_add(requested);
        let growth = end.saturating_sub(self.high_water);
        if growth > 0
            && let Err(error) = self.budget.borrow_mut().charge_temporary(growth)
        {
            *self.error.borrow_mut() = Some(error);
            return Err(io::Error::other("archive temporary-space budget exceeded"));
        }
        let written = self.inner.write(bytes)?;
        self.position = self
            .position
            .saturating_add(u64::try_from(written).unwrap_or(u64::MAX));
        self.high_water = self.high_water.max(end);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Seek for BudgetedWriteSeek {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.position = self.inner.seek(position)?;
        Ok(self.position)
    }
}

fn write_zip(
    stage: BudgetedWriteSeek,
    entries: &[CreateEntry],
    encrypted: bool,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
) -> Result<(), ArchiveOperationError> {
    let codec_name_bytes = entries.iter().fold(0_u64, |total, entry| {
        total
            .saturating_add(u64::try_from(entry.archive_name.len()).unwrap_or(u64::MAX))
            .saturating_add(u64::from(matches!(entry.kind, CreateEntryKind::Directory)))
    });
    let _codec_names = stage.reserve_memory(codec_name_bytes)?;
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
    writer.finish().map_err(map_zip_error)?.sync_all()
}

enum TarEncoder {
    Plain,
    Gzip,
    Zstd,
}

fn write_tar(
    stage: BudgetedWriteSeek,
    entries: &[CreateEntry],
    encoder: TarEncoder,
    cancellation: &CancellationToken,
) -> Result<(), ArchiveOperationError> {
    match encoder {
        TarEncoder::Plain => write_tar_stream(stage, entries, cancellation)?.sync_all(),
        TarEncoder::Gzip => {
            let encoder = flate2::write::GzEncoder::new(stage, flate2::Compression::default());
            let encoder = write_tar_stream(encoder, entries, cancellation)?;
            encoder.finish().map_err(|error| map_io(&error))?.sync_all()
        }
        TarEncoder::Zstd => {
            let encoder =
                zstd::stream::write::Encoder::new(stage, 0).map_err(|error| map_io(&error))?;
            let encoder = write_tar_stream(encoder, entries, cancellation)?;
            encoder.finish().map_err(|error| map_io(&error))?.sync_all()
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
        let name = Path::new(std::ffi::OsStr::from_bytes(&entry.archive_name));
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
    stage: BudgetedWriteSeek,
    entries: &[CreateEntry],
    encrypted: bool,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
) -> Result<(), ArchiveOperationError> {
    let codec_name_bytes = entries.iter().fold(0_u64, |total, entry| {
        total.saturating_add(u64::try_from(entry.archive_name.len()).unwrap_or(u64::MAX))
    });
    let _codec_names = stage.reserve_memory(codec_name_bytes)?;
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
    writer.finish().map_err(|error| map_io(&error))?.sync_all()
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn append_archive_phase<S: JournalStorage>(
    journal: &mut Journal<S>,
    job_id: JobId,
    generation: EventGeneration,
    phase: JournalPhase,
    plan: &ArchiveOperationPlan,
    staging: &Path,
    destination_before: Option<ArchivePathIdentity>,
    destination_after: Option<ArchivePathIdentity>,
) -> Result<(), ArchiveOperationError> {
    let checkpoint = ArchiveCheckpoint::new(
        plan.clone(),
        musheen_core::StorePath::from_unix_path(staging.as_os_str()),
        path_identity(staging)?,
        destination_before,
        destination_after,
    );
    journal
        .append_archive(
            job_id,
            generation,
            phase,
            Durability::CrashDurable,
            checkpoint,
        )
        .map(|_| ())
        .map_err(|_| ArchiveOperationError::Journal)
}

pub(crate) fn path_identity(
    path: &Path,
) -> Result<Option<ArchivePathIdentity>, ArchiveOperationError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(map_io(&error)),
    };
    if metadata.file_type().is_symlink() {
        return Err(ArchiveOperationError::UnsupportedFileType);
    }
    Ok(Some(ArchivePathIdentity::new(
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.is_dir(),
    )))
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
    .map_err(|error| map_io(&error))?;
    sync_parent(path)
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
