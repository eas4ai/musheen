use super::budget::{
    ArchiveBudget, ArchiveMemoryLease, ArchiveOperationAccounting, ArchiveOperationError,
    ArchiveOperationLimits, map_io,
};
use super::{ArchivePassword, ArchivePasswordProvider, PasswordRequest};
use musheen_core::CancellationToken;
use musheen_ops::{
    ArchiveCheckpoint, ArchiveCleanupKind, ArchiveCodec, ArchiveConflictPolicy, ArchiveEventPhase,
    ArchiveOperationPlan, ArchivePathIdentity, Clock, Durability, EventGeneration, JobId, Journal,
    JournalPhase, JournalStorage, OperationKind, ScheduledJob, Scheduler, SchedulerError,
    StagingPath,
};
use nix::libc::O_NOFOLLOW;
use sevenz_rust2::encoder_options::{AesEncoderOptions, Lzma2Options, LzmaOptions};
use sevenz_rust2::{ArchiveEntry as SevenEntry, ArchiveWriter as SevenWriter, Password};
use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use zip::write::SimpleFileOptions;

use super::workspace::{
    WorkspaceWriter, deflate_encoder_workspace_bytes, gzip_encoder_workspace_bytes,
    zstd_encoder_workspace_bytes,
};

const ZIP_ENTRY_STATE_BYTES: u64 = 512;
const SEVEN_HEADER_INITIAL_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveOperationOutcome {
    Published,
    Skipped,
}

pub fn execute_scheduled_archive_operation<C: Clock, S: JournalStorage>(
    scheduler: &Scheduler<C>,
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

pub fn execute_archive_plan<S: JournalStorage>(
    plan: &ArchiveOperationPlan,
    limits: &ArchiveOperationLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
    journal: &mut Journal<S>,
    job_id: JobId,
    generation: EventGeneration,
) -> Result<ArchiveOperationOutcome, ArchiveOperationError> {
    let mut report_phase = |_| Ok(());
    let mut begin_commit = || {
        cancellation
            .wait_if_paused()
            .map_err(|_| ArchiveOperationError::Cancelled)
    };
    execute_archive_operation(
        plan,
        limits,
        passwords,
        cancellation,
        journal,
        job_id,
        generation,
        &mut report_phase,
        &mut begin_commit,
        &ArchiveOperationAccounting::default(),
    )
}

pub fn execute_scheduled_archive_operation_with_accounting<C: Clock, S: JournalStorage>(
    scheduler: &Scheduler<C>,
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
    let mut begin_commit = || loop {
        match scheduler.begin_commit(job_id) {
            Ok(()) => return Ok(()),
            Err(SchedulerError::CommitAdmissionDenied(_)) if cancellation.is_paused() => {
                cancellation.wait_if_paused()?;
            }
            Err(SchedulerError::CommitAdmissionDenied(_)) if cancellation.is_cancelled() => {
                return Err(ArchiveOperationError::Cancelled);
            }
            Err(_) => return Err(ArchiveOperationError::Engine),
        }
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
        &mut begin_commit,
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
    begin_commit: &mut dyn FnMut() -> Result<(), ArchiveOperationError>,
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
            begin_commit,
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
            begin_commit,
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
    begin_commit: &mut dyn FnMut() -> Result<(), ArchiveOperationError>,
    accounting: &ArchiveOperationAccounting,
) -> Result<ArchiveOperationOutcome, ArchiveOperationError> {
    report_phase(ArchiveEventPhase::Preflight)?;
    cancellation.wait_if_paused()?;
    let destination = local_path(plan.destination())?;
    if let Some(outcome) = existing_destination_outcome(&destination, plan.conflict_policy())? {
        return Ok(outcome);
    }
    let staging_parent = destination
        .parent()
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive destination needs a parent",
        ))?;
    let operation_limits = limits.for_staging(staging_parent)?;
    let budget = Rc::new(RefCell::new(
        ArchiveBudget::with_accounting(operation_limits, accounting.clone())
            .with_identity_cancellation(cancellation.clone()),
    ));
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
    let entries = collect_create_entries(plan, &mut budget.borrow_mut(), cancellation)?;
    let staging = staging_path(plan, job_id, generation)?;
    let rollback = cleanup_path(&staging)?;
    let destination_before =
        path_identity_with_controls(&destination, Some(&budget.borrow()), Some(cancellation))?;
    append_archive_phase(
        journal,
        job_id,
        generation,
        JournalPhase::Planned,
        plan,
        &staging,
        destination_before,
        None,
        Some(&budget.borrow()),
    )?;
    let mut published = false;
    let mut transaction_started = false;
    let mut stage_root = None;
    let mut result = (|| {
        let stage_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staging)
            .map_err(|error| map_io(&error))?;
        sync_parent(&staging)?;
        stage_root =
            path_identity_with_controls(&staging, Some(&budget.borrow()), Some(cancellation))?;
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
            Some(&budget.borrow()),
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
            Some(&budget.borrow()),
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
            Some(&budget.borrow()),
        )?;
        report_phase(ArchiveEventPhase::Publishing)?;
        cancellation.wait_if_paused()?;
        let staging_before_publish =
            path_identity_with_controls(&staging, Some(&budget.borrow()), Some(cancellation))?
                .ok_or(ArchiveOperationError::Conflict)?;
        begin_commit()?;
        transaction_started = true;
        let outcome = {
            let rollback_quarantine = deletion_path(&rollback)?;
            let payload_quarantine = publication_quarantine_path(&staging)?;
            let publication = ArchivePublicationPaths::new(
                &staging,
                &rollback,
                &rollback_quarantine,
                &payload_quarantine,
                destination_before,
            );
            let mut publication_checkpoint = |phase, destination_after, paths| {
                append_archive_phase(
                    journal,
                    job_id,
                    generation,
                    publication_phase(phase, paths),
                    plan,
                    &staging,
                    destination_before,
                    destination_after,
                    Some(&budget.borrow()),
                )
            };
            publish_staging(
                publication,
                &destination,
                plan.conflict_policy(),
                staging_before_publish,
                destination_before,
                Some(&budget.borrow()),
                Some(cancellation),
                &mut publication_checkpoint,
            )?
        };
        if outcome == ArchiveOperationOutcome::Skipped {
            if path_identity_with_controls(&staging, Some(&budget.borrow()), Some(cancellation))?
                != Some(staging_before_publish)
            {
                return Err(ArchiveOperationError::Conflict);
            }
            let stage_deletion = deletion_path(&staging)?;
            let cleanup = ArchiveCleanupIntent::new(
                ArchiveCleanupKind::PrepublishStage,
                &staging,
                &stage_deletion,
                Some(staging_before_publish),
            );
            append_archive_phase(
                journal,
                job_id,
                generation,
                cleanup_phase(JournalPhase::PrepublishStageCleanupPlanned, cleanup),
                plan,
                &staging,
                destination_before,
                None,
                Some(&budget.borrow()),
            )?;
            {
                let mut quarantined = || {
                    append_archive_phase(
                        journal,
                        job_id,
                        generation,
                        cleanup_phase(JournalPhase::PrepublishStageCleanupQuarantined, cleanup),
                        plan,
                        &staging,
                        destination_before,
                        None,
                        Some(&budget.borrow()),
                    )
                };
                remove_owned_journaled(
                    &staging,
                    &stage_deletion,
                    Some(staging_before_publish),
                    &budget.borrow(),
                    cancellation,
                    &mut quarantined,
                )
                .map_err(|_| ArchiveOperationError::RecoveryRequired)?;
            }
            append_archive_phase(
                journal,
                job_id,
                generation,
                JournalPhase::RolledBack,
                plan,
                &staging,
                destination_before,
                None,
                Some(&budget.borrow()),
            )?;
            return Ok(outcome);
        }
        published = true;
        sync_parent(&destination)?;
        let destination_after =
            path_identity_with_controls(&destination, Some(&budget.borrow()), Some(cancellation))?;
        let rollback_deletion = deletion_path(&rollback)?;
        let cleanup = ArchiveCleanupIntent::new(
            ArchiveCleanupKind::PublishedDestination,
            &rollback,
            &rollback_deletion,
            destination_before,
        );
        append_archive_phase(
            journal,
            job_id,
            generation,
            cleanup_phase(JournalPhase::PublishedDestinationCleanupPlanned, cleanup),
            plan,
            &staging,
            destination_before,
            destination_after,
            Some(&budget.borrow()),
        )?;
        report_phase(ArchiveEventPhase::Cleaning)?;
        if path_identity_with_controls(&rollback, Some(&budget.borrow()), Some(cancellation))?
            != destination_before
        {
            return Err(ArchiveOperationError::RecoveryRequired);
        }
        {
            let mut quarantined = || {
                append_archive_phase(
                    journal,
                    job_id,
                    generation,
                    cleanup_phase(
                        JournalPhase::PublishedDestinationCleanupQuarantined,
                        cleanup,
                    ),
                    plan,
                    &staging,
                    destination_before,
                    destination_after,
                    Some(&budget.borrow()),
                )
            };
            remove_owned_journaled(
                &rollback,
                &rollback_deletion,
                destination_before,
                &budget.borrow(),
                cancellation,
                &mut quarantined,
            )
            .map_err(|_| ArchiveOperationError::RecoveryRequired)?;
        }
        append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::StagingCleaned,
            plan,
            &staging,
            destination_before,
            destination_after,
            Some(&budget.borrow()),
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
            Some(&budget.borrow()),
        )?;
        Ok(ArchiveOperationOutcome::Published)
    })();

    if !matches!(result, Err(ArchiveOperationError::RecoveryRequired))
        && result.is_err()
        && !published
        && !transaction_started
        && staging.exists()
    {
        let cleanup_cancellation = CancellationToken::new();
        let cleanup_budget = budget
            .borrow()
            .next_phase()
            .with_identity_cancellation(cleanup_cancellation.clone());
        let stage_deletion = deletion_path(&staging)?;
        let cleanup_result = path_identity_with_controls(
            &staging,
            Some(&cleanup_budget),
            Some(&cleanup_cancellation),
        )
        .and_then(|cleanup_identity| {
            let cleanup_owned =
                stage_root
                    .zip(cleanup_identity)
                    .is_some_and(|(created, current)| {
                        created.device() == current.device() && created.inode() == current.inode()
                    });
            if !cleanup_owned {
                return Err(ArchiveOperationError::RecoveryRequired);
            }
            let cleanup = ArchiveCleanupIntent::new(
                ArchiveCleanupKind::PrepublishStage,
                &staging,
                &stage_deletion,
                cleanup_identity,
            );
            append_archive_phase(
                journal,
                job_id,
                generation,
                cleanup_phase(JournalPhase::PrepublishStageCleanupPlanned, cleanup),
                plan,
                &staging,
                destination_before,
                None,
                Some(&cleanup_budget),
            )
            .and_then(|()| {
                let mut quarantined = || {
                    append_archive_phase(
                        journal,
                        job_id,
                        generation,
                        cleanup_phase(JournalPhase::PrepublishStageCleanupQuarantined, cleanup),
                        plan,
                        &staging,
                        destination_before,
                        None,
                        Some(&cleanup_budget),
                    )
                };
                remove_owned_journaled(
                    &staging,
                    &stage_deletion,
                    cleanup_identity,
                    &cleanup_budget,
                    &cleanup_cancellation,
                    &mut quarantined,
                )
            })
        });
        if cleanup_result.is_ok() {
            if append_archive_phase(
                journal,
                job_id,
                generation,
                JournalPhase::RolledBack,
                plan,
                &staging,
                destination_before,
                None,
                Some(&cleanup_budget),
            )
            .is_err()
            {
                result = Err(ArchiveOperationError::RecoveryRequired);
            }
        } else {
            result = Err(ArchiveOperationError::RecoveryRequired);
        }
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
            cancellation.wait_if_paused()?;
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
        if let Err(error) = self
            .budget
            .borrow()
            .check_stage_write(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
        {
            *self.error.borrow_mut() = Some(map_io(&error));
            return Err(error);
        }
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
    // ZipWriter retains one ZipFileData, two owned name copies, and one map slot per
    // entry. The fixed bound exceeds the locked zip 6 structure plus its map bucket.
    let entry_state = u64::try_from(entries.len())
        .unwrap_or(u64::MAX)
        .saturating_mul(ZIP_ENTRY_STATE_BYTES);
    let codec_workspace = u64::try_from(deflate_encoder_workspace_bytes()).unwrap_or(u64::MAX);
    let _codec_memory = stage.reserve_memory(
        codec_workspace
            .saturating_add(entry_state)
            .saturating_add(codec_name_bytes.saturating_mul(2)),
    )?;
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
        cancellation.wait_if_paused()?;
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
            let workspace = stage.reserve_memory(
                u64::try_from(gzip_encoder_workspace_bytes()).unwrap_or(u64::MAX),
            )?;
            let stage = WorkspaceWriter::new(stage, workspace);
            let encoder = flate2::write::GzEncoder::new(stage, flate2::Compression::default());
            let encoder = write_tar_stream(encoder, entries, cancellation)?;
            encoder
                .finish()
                .map_err(|error| map_io(&error))?
                .into_inner()
                .sync_all()
        }
        TarEncoder::Zstd => {
            let workspace = stage.reserve_memory(
                u64::try_from(zstd_encoder_workspace_bytes()?).unwrap_or(u64::MAX),
            )?;
            let stage = WorkspaceWriter::new(stage, workspace);
            let encoder =
                zstd::stream::write::Encoder::new(stage, 0).map_err(|error| map_io(&error))?;
            let encoder = write_tar_stream(encoder, entries, cancellation)?;
            encoder
                .finish()
                .map_err(|error| map_io(&error))?
                .into_inner()
                .sync_all()
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
        cancellation.wait_if_paused()?;
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
    let content_options = Lzma2Options::default();
    let header_options = LzmaOptions::default();
    let entry_count = u64::try_from(entries.len()).unwrap_or(u64::MAX);
    let retained_entries = entry_count
        .saturating_mul(u64::try_from(std::mem::size_of::<SevenEntry>()).unwrap_or(u64::MAX))
        .saturating_add(codec_name_bytes);
    // The writer creates two 64 KiB header vectors and an encoded vector sized to
    // half the serialized header. Names are UTF-16 in that representation.
    let serialized_header = SEVEN_HEADER_INITIAL_BYTES
        .saturating_add(codec_name_bytes.saturating_mul(2))
        .saturating_add(entry_count.saturating_mul(512));
    let header_buffers = serialized_header.saturating_mul(3);
    let active_workspace = content_options.memory_usage_bytes().max(
        header_options
            .memory_usage_bytes()
            .saturating_add(header_buffers),
    );
    let _codec_memory = stage.reserve_memory(retained_entries.saturating_add(active_workspace))?;
    let password = encrypted
        .then(|| requested_password(ArchiveCodec::SevenZip, passwords))
        .transpose()?;
    let mut writer = SevenWriter::new(stage).map_err(map_seven_create_error)?;
    writer
        .reserve_entries_exact(entries.len())
        .map_err(map_seven_create_error)?;
    if let Some(password) = password.as_ref() {
        let password = std::str::from_utf8(password.as_bytes())
            .map_err(|_| ArchiveOperationError::InvalidPassword)?;
        writer.set_content_methods(vec![
            AesEncoderOptions::new(Password::new(password)).into(),
            content_options.clone().into(),
        ]);
    } else {
        writer.set_content_methods(vec![content_options.into()]);
    }
    for entry in entries {
        cancellation.wait_if_paused()?;
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
        if self.cancellation.wait_if_paused().is_err() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "archive operation cancelled",
            ));
        }
        self.inner.read(buffer)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ArchiveCleanupIntent<'a> {
    kind: ArchiveCleanupKind,
    source: &'a Path,
    quarantine: &'a Path,
    identity: Option<ArchivePathIdentity>,
}

impl<'a> ArchiveCleanupIntent<'a> {
    pub(crate) const fn new(
        kind: ArchiveCleanupKind,
        source: &'a Path,
        quarantine: &'a Path,
        identity: Option<ArchivePathIdentity>,
    ) -> Self {
        Self {
            kind,
            source,
            quarantine,
            identity,
        }
    }

    pub(crate) const fn source(self) -> &'a Path {
        self.source
    }

    pub(crate) const fn quarantine(self) -> &'a Path {
        self.quarantine
    }

    pub(crate) const fn identity(self) -> Option<ArchivePathIdentity> {
        self.identity
    }
}

pub(crate) struct ArchiveJournalPhase<'a> {
    phase: JournalPhase,
    cleanup: Option<ArchiveCleanupIntent<'a>>,
    publication: Option<ArchivePublicationPaths<'a>>,
}

impl From<JournalPhase> for ArchiveJournalPhase<'_> {
    fn from(phase: JournalPhase) -> Self {
        Self {
            phase,
            cleanup: None,
            publication: None,
        }
    }
}

pub(crate) const fn cleanup_phase(
    phase: JournalPhase,
    cleanup: ArchiveCleanupIntent<'_>,
) -> ArchiveJournalPhase<'_> {
    ArchiveJournalPhase {
        phase,
        cleanup: Some(cleanup),
        publication: None,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ArchivePublicationPaths<'a> {
    source: &'a Path,
    rollback: &'a Path,
    rollback_quarantine: &'a Path,
    payload_quarantine: &'a Path,
    rollback_identity: Option<ArchivePathIdentity>,
}

impl<'a> ArchivePublicationPaths<'a> {
    pub(crate) const fn new(
        source: &'a Path,
        rollback: &'a Path,
        rollback_quarantine: &'a Path,
        payload_quarantine: &'a Path,
        rollback_identity: Option<ArchivePathIdentity>,
    ) -> Self {
        Self {
            source,
            rollback,
            rollback_quarantine,
            payload_quarantine,
            rollback_identity,
        }
    }
}

pub(crate) const fn publication_phase<'a>(
    phase: JournalPhase,
    publication: ArchivePublicationPaths<'a>,
) -> ArchiveJournalPhase<'a> {
    ArchiveJournalPhase {
        phase,
        cleanup: None,
        publication: Some(publication),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn append_archive_phase<'a, S: JournalStorage>(
    journal: &mut Journal<S>,
    job_id: JobId,
    generation: EventGeneration,
    phase: impl Into<ArchiveJournalPhase<'a>>,
    plan: &ArchiveOperationPlan,
    staging: &Path,
    destination_before: Option<ArchivePathIdentity>,
    destination_after: Option<ArchivePathIdentity>,
    budget: Option<&ArchiveBudget>,
) -> Result<(), ArchiveOperationError> {
    let phase = phase.into();
    let cleanup_intent = phase.cleanup;
    let publication_intent = phase.publication;
    let phase = phase.phase;
    let include_plan = !journal.records().iter().any(|record| {
        record.job_id() == job_id
            && record.generation() == generation
            && record.archive_checkpoint().is_some()
    });
    let _checkpoint_memory = budget
        .map(|budget| {
            budget.reserve_memory(checkpoint_memory_upper_bound(
                plan,
                staging.as_os_str().len(),
                include_plan,
            ))
        })
        .transpose()?;
    let prior_checkpoint = journal
        .records()
        .iter()
        .rev()
        .find(|record| record.job_id() == job_id && record.generation() == generation)
        .and_then(|record| record.archive_checkpoint());
    let shared_plan =
        prior_checkpoint.map(|checkpoint| (checkpoint.shared_plan(), checkpoint.plan_digest()));
    let identity_memory_limit = budget.map_or_else(
        || {
            prior_checkpoint.map_or(
                ArchiveOperationLimits::default().max_memory_bytes,
                |checkpoint| checkpoint.identity_memory_limit(),
            )
        },
        ArchiveBudget::max_memory_bytes,
    );
    let staging_store = musheen_core::StorePath::from_unix_path(staging.as_os_str());
    let explicit_cleanup_phase = matches!(
        phase,
        JournalPhase::PrepublishStageCleanupPlanned
            | JournalPhase::PrepublishStageCleanupQuarantined
            | JournalPhase::PublishedDestinationCleanupPlanned
            | JournalPhase::PublishedDestinationCleanupQuarantined
    );
    let observed_staging_identity = if explicit_cleanup_phase {
        None
    } else {
        path_identity_with_controls(staging, budget, None)?
    };
    let preserve_staging_identity = matches!(
        phase,
        JournalPhase::DestinationQuarantinePlanned
            | JournalPhase::DestinationQuarantined
            | JournalPhase::StagePublishPlanned
            | JournalPhase::DestinationPublished
            | JournalPhase::PublishRollbackPlanned
            | JournalPhase::PublishedPayloadQuarantined
            | JournalPhase::DestinationRestorePlanned
            | JournalPhase::DestinationRestored
            | JournalPhase::PrepublishStageCleanupPlanned
            | JournalPhase::PrepublishStageCleanupQuarantined
            | JournalPhase::PublishedDestinationCleanupPlanned
            | JournalPhase::PublishedDestinationCleanupQuarantined
            | JournalPhase::StagingCleaned
            | JournalPhase::RecoveryRequired
            | JournalPhase::Completed
    );
    let staging_identity = if preserve_staging_identity {
        prior_checkpoint
            .and_then(ArchiveCheckpoint::staging_identity)
            .or(observed_staging_identity)
    } else {
        observed_staging_identity
    };
    let destination_after = destination_after.or_else(|| {
        (phase == JournalPhase::RecoveryRequired)
            .then(|| prior_checkpoint.and_then(ArchiveCheckpoint::destination_after))
            .flatten()
    });
    let checkpoint = if let Some((shared_plan, plan_digest)) = shared_plan {
        ArchiveCheckpoint::from_shared_plan(
            shared_plan,
            plan_digest,
            staging_store.clone(),
            staging_identity,
            destination_before,
            destination_after,
        )
    } else {
        ArchiveCheckpoint::new(
            plan.clone(),
            staging_store.clone(),
            staging_identity,
            destination_before,
            destination_after,
        )
    };
    let nonce = StagingPath::nonce(&staging_store).ok_or(ArchiveOperationError::UnsafePath(
        "archive staging path has no ownership nonce",
    ))?;
    let stage_deletion = deletion_path(staging)?;
    let publication_quarantine = publication_intent.map_or_else(
        || {
            prior_checkpoint.map_or_else(
                || publication_quarantine_path(staging),
                |checkpoint| {
                    checkpoint
                        .publication_quarantine()
                        .map(local_path)
                        .transpose()
                        .and_then(|path| {
                            path.map_or_else(|| publication_quarantine_path(staging), Ok)
                        })
                },
            )
        },
        |intent| Ok(intent.payload_quarantine.to_path_buf()),
    )?;
    let required_cleanup_kind = match phase {
        JournalPhase::PrepublishStageCleanupPlanned
        | JournalPhase::PrepublishStageCleanupQuarantined => {
            Some(ArchiveCleanupKind::PrepublishStage)
        }
        JournalPhase::PublishedDestinationCleanupPlanned
        | JournalPhase::PublishedDestinationCleanupQuarantined => {
            Some(ArchiveCleanupKind::PublishedDestination)
        }
        _ => None,
    };
    if required_cleanup_kind != cleanup_intent.map(|intent| intent.kind) {
        return Err(ArchiveOperationError::InvalidArchive);
    }
    let publication_phase = matches!(
        phase,
        JournalPhase::DestinationQuarantinePlanned
            | JournalPhase::DestinationQuarantined
            | JournalPhase::StagePublishPlanned
            | JournalPhase::DestinationPublished
            | JournalPhase::PublishRollbackPlanned
            | JournalPhase::PublishedPayloadQuarantined
            | JournalPhase::DestinationRestorePlanned
            | JournalPhase::DestinationRestored
    );
    if publication_intent.is_some() && !publication_phase {
        return Err(ArchiveOperationError::InvalidArchive);
    }
    if let Some(intent) = cleanup_intent
        && (intent.source == intent.quarantine
            || intent.source.parent().is_none()
            || intent.source.parent() != intent.quarantine.parent())
    {
        return Err(ArchiveOperationError::UnsafePath(
            "archive cleanup source and quarantine must be distinct siblings",
        ));
    }
    if let Some(intent) = publication_intent {
        let parent = staging.parent();
        if parent.is_none()
            || intent.source.parent() != parent
            || intent.rollback.parent() != parent
            || intent.rollback_quarantine.parent() != parent
            || intent.payload_quarantine.parent() != parent
            || intent.rollback == intent.rollback_quarantine
            || intent.rollback == intent.payload_quarantine
            || intent.rollback_quarantine == intent.payload_quarantine
        {
            return Err(ArchiveOperationError::UnsafePath(
                "archive publication paths must be distinct siblings",
            ));
        }
    }
    let checkpoint = checkpoint
        .with_staging_nonce(nonce)
        .with_stage_deletion(musheen_core::StorePath::from_unix_path(
            stage_deletion.as_os_str(),
        ))
        .with_publication_quarantine(musheen_core::StorePath::from_unix_path(
            publication_quarantine.as_os_str(),
        ))
        .with_identity_memory_limit(identity_memory_limit)
        .with_identity_timeout_millis(budget.map_or_else(
            || prior_checkpoint.map_or(30_000, |checkpoint| checkpoint.identity_timeout_millis()),
            ArchiveBudget::max_identity_millis,
        ));
    let checkpoint = if let Some(intent) = cleanup_intent {
        checkpoint.with_cleanup_intent(
            intent.kind,
            musheen_core::StorePath::from_unix_path(intent.source.as_os_str()),
            musheen_core::StorePath::from_unix_path(intent.quarantine.as_os_str()),
            intent.identity,
        )
    } else if let Some(intent) = publication_intent {
        checkpoint.with_cleanup_intent(
            ArchiveCleanupKind::PublishedDestination,
            musheen_core::StorePath::from_unix_path(intent.rollback.as_os_str()),
            musheen_core::StorePath::from_unix_path(intent.rollback_quarantine.as_os_str()),
            intent.rollback_identity,
        )
    } else if let Some(prior) = prior_checkpoint
        && let (Some(kind), Some(source), Some(quarantine)) = (
            prior.cleanup_kind(),
            prior.cleanup(),
            prior.cleanup_deletion(),
        )
    {
        checkpoint.with_cleanup_intent(
            kind,
            source.clone(),
            quarantine.clone(),
            prior.cleanup_identity(),
        )
    } else {
        checkpoint
    };
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

pub(crate) fn cleanup_path(staging: &Path) -> Result<PathBuf, ArchiveOperationError> {
    let name = staging
        .file_name()
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive staging needs a file name",
        ))?;
    let mut cleanup_name = name.to_os_string();
    cleanup_name.push(".rollback");
    Ok(staging.with_file_name(cleanup_name))
}

pub(crate) fn publication_quarantine_path(
    staging: &Path,
) -> Result<PathBuf, ArchiveOperationError> {
    let name = staging
        .file_name()
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive staging needs a file name",
        ))?;
    let mut quarantine_name = name.to_os_string();
    quarantine_name.push(".published");
    Ok(staging.with_file_name(quarantine_name))
}

fn checkpoint_memory_upper_bound(
    plan: &ArchiveOperationPlan,
    staging_path_bytes: usize,
    include_plan: bool,
) -> u64 {
    let path_bytes = plan
        .sources()
        .iter()
        .chain(std::iter::once(plan.destination()))
        .map(|path| {
            path.as_unix_path()
                .map_or(0_u64, |value| value.as_os_str().len() as u64)
        })
        .fold(0_u64, u64::saturating_add);
    let paths = u64::try_from(plan.sources().len().saturating_add(1)).unwrap_or(u64::MAX);
    // StorePath's byte-array JSON and the outer escaped envelope use at most eight bytes per input
    // path byte. The fixed allowance covers all record fields, both identities, and checksums.
    let record = u64::try_from(staging_path_bytes)
        .unwrap_or(u64::MAX)
        .saturating_mul(8)
        .saturating_add(8 * 1_024);
    if include_plan {
        // A JSON byte may expand to six bytes in the payload and six more when the payload is
        // escaped into the checksum envelope. Include the owned plan and collection storage too.
        record
            .saturating_add(path_bytes.saturating_mul(37))
            .saturating_add(paths.saturating_mul(512))
    } else {
        record
    }
}

pub(crate) fn path_identity_with_controls(
    path: &Path,
    budget: Option<&ArchiveBudget>,
    cancellation: Option<&CancellationToken>,
) -> Result<Option<ArchivePathIdentity>, ArchiveOperationError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(map_io(&error)),
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink()
        || file_type.is_fifo()
        || file_type.is_socket()
        || file_type.is_block_device()
        || file_type.is_char_device()
    {
        return Err(ArchiveOperationError::UnsupportedFileType);
    }
    let parent_path = path.parent().ok_or(ArchiveOperationError::UnsafePath(
        "identity path needs a parent",
    ))?;
    let name = path.file_name().ok_or(ArchiveOperationError::UnsafePath(
        "identity path needs a file name",
    ))?;
    let parent = rustix::fs::open(
        parent_path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(map_errno)?;
    let opened = rustix::fs::openat(
        &parent,
        name,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(map_errno)?;
    let opened = File::from(opened);
    let opened_metadata = opened.metadata().map_err(|error| map_io(&error))?;
    if opened_metadata.dev() != metadata.dev() || opened_metadata.ino() != metadata.ino() {
        return Err(ArchiveOperationError::Conflict);
    }
    let digest = content_digest(
        opened,
        &metadata,
        path.as_os_str().len(),
        budget,
        cancellation,
    )?;
    Ok(Some(
        ArchivePathIdentity::new(
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.is_dir(),
        )
        .with_content_digest(digest),
    ))
}

pub(crate) fn file_identity(file: &File) -> Result<ArchivePathIdentity, ArchiveOperationError> {
    let metadata = file.metadata().map_err(|error| map_io(&error))?;
    if !metadata.is_file() {
        return Err(ArchiveOperationError::UnsupportedFileType);
    }
    let mut reader = file.try_clone().map_err(|error| map_io(&error))?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| map_io(&error))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&metadata.mode().to_le_bytes());
    hasher.update(&metadata.len().to_le_bytes());
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(|error| map_io(&error))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(ArchivePathIdentity::new(
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        false,
    )
    .with_content_digest(*hasher.finalize().as_bytes()))
}

fn content_digest(
    opened: File,
    metadata: &std::fs::Metadata,
    path_bytes: usize,
    budget: Option<&ArchiveBudget>,
    cancellation: Option<&CancellationToken>,
) -> Result<[u8; 32], ArchiveOperationError> {
    let mut hasher = blake3::Hasher::new();
    let deadline = budget.map(ArchiveBudget::identity_walk_deadline);
    hash_path_content(
        opened,
        metadata,
        path_bytes,
        &mut hasher,
        budget,
        cancellation,
        deadline,
    )?;
    Ok(*hasher.finalize().as_bytes())
}

struct IdentityNode {
    opened: File,
    metadata: std::fs::Metadata,
    name: Option<Vec<u8>>,
    depth: usize,
    _memory: Option<ArchiveMemoryLease>,
}

fn hash_path_content(
    opened: File,
    metadata: &std::fs::Metadata,
    path_bytes: usize,
    hasher: &mut blake3::Hasher,
    budget: Option<&ArchiveBudget>,
    cancellation: Option<&CancellationToken>,
    deadline: Option<std::time::Instant>,
) -> Result<(), ArchiveOperationError> {
    const MAX_IDENTITY_DEPTH: usize = 4_096;
    let root_memory = budget
        .map(|budget| {
            budget.reserve_memory(u64::try_from(path_bytes.saturating_add(512)).unwrap_or(u64::MAX))
        })
        .transpose()?;
    let mut stack = vec![IdentityNode {
        opened,
        metadata: metadata.clone(),
        name: None,
        depth: 0,
        _memory: root_memory,
    }];
    while let Some(node) = stack.pop() {
        if let Some(budget) = budget {
            budget.identity_checkpoint(deadline.expect("budgeted identity walk has deadline"))?;
        }
        if let Some(cancellation) = cancellation {
            cancellation.wait_if_paused()?;
        }
        if node.depth > MAX_IDENTITY_DEPTH {
            return Err(ArchiveOperationError::LimitExceeded {
                resource: "identity traversal depth",
                value: u64::try_from(node.depth).unwrap_or(u64::MAX),
                maximum: MAX_IDENTITY_DEPTH as u64,
            });
        }
        if let Some(name) = &node.name {
            hasher.update(&(name.len() as u64).to_le_bytes());
            hasher.update(name);
        }
        hasher.update(&node.metadata.mode().to_le_bytes());
        hasher.update(&node.metadata.len().to_le_bytes());
        if node.metadata.is_file() {
            let mut file = node.opened;
            let buffer_memory = budget
                .map(|budget| budget.reserve_memory(8 * 1_024))
                .transpose()?;
            let mut buffer = [0_u8; 8 * 1024];
            loop {
                if let Some(budget) = budget {
                    budget.identity_checkpoint(
                        deadline.expect("budgeted identity walk has deadline"),
                    )?;
                }
                if let Some(cancellation) = cancellation {
                    cancellation.wait_if_paused()?;
                }
                let count = file.read(&mut buffer).map_err(|error| map_io(&error))?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
            drop(buffer_memory);
            continue;
        }
        if !node.metadata.is_dir() {
            return Err(ArchiveOperationError::UnsupportedFileType);
        }
        let mut children = Vec::new();
        let directory_path = proc_fd_path(&node.opened);
        for child in std::fs::read_dir(&directory_path).map_err(|error| map_io(&error))? {
            if let Some(budget) = budget {
                budget
                    .identity_checkpoint(deadline.expect("budgeted identity walk has deadline"))?;
            }
            if let Some(cancellation) = cancellation {
                cancellation.wait_if_paused()?;
            }
            let child = child.map_err(|error| map_io(&error))?;
            let child_name = child.file_name();
            let memory = budget
                .map(|budget| {
                    budget.reserve_memory(
                        u64::try_from(
                            child_name
                                .as_bytes()
                                .len()
                                .saturating_add(path_bytes)
                                .saturating_add(512),
                        )
                        .unwrap_or(u64::MAX),
                    )
                })
                .transpose()?;
            let name = child_name.as_bytes().to_vec();
            let child_path = directory_path.join(&child_name);
            let child_metadata =
                std::fs::symlink_metadata(&child_path).map_err(|error| map_io(&error))?;
            let file_type = child_metadata.file_type();
            if file_type.is_symlink()
                || file_type.is_fifo()
                || file_type.is_socket()
                || file_type.is_block_device()
                || file_type.is_char_device()
            {
                return Err(ArchiveOperationError::UnsupportedFileType);
            }
            let opened = rustix::fs::openat(
                &node.opened,
                &child_name,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(map_errno)?;
            let opened = File::from(opened);
            let opened_metadata = opened.metadata().map_err(|error| map_io(&error))?;
            if opened_metadata.dev() != child_metadata.dev()
                || opened_metadata.ino() != child_metadata.ino()
            {
                return Err(ArchiveOperationError::Conflict);
            }
            children.push(IdentityNode {
                opened,
                metadata: child_metadata,
                name: Some(name),
                depth: node.depth.saturating_add(1),
                _memory: memory,
            });
        }
        children.sort_by(|left, right| left.name.cmp(&right.name));
        stack.extend(children.into_iter().rev());
    }
    Ok(())
}

fn proc_fd_path(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

pub(crate) fn staging_path(
    plan: &ArchiveOperationPlan,
    job_id: JobId,
    generation: EventGeneration,
) -> Result<PathBuf, ArchiveOperationError> {
    let mut nonce = [0_u8; 16];
    File::open("/dev/urandom")
        .and_then(|mut random| random.read_exact(&mut nonce))
        .map_err(|error| map_io(&error))?;
    StagingPath::for_destination_with_nonce(plan.destination(), job_id, generation, nonce)
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn publish_staging<'a>(
    paths: ArchivePublicationPaths<'a>,
    destination: &Path,
    policy: ArchiveConflictPolicy,
    expected_staging: ArchivePathIdentity,
    expected_destination: Option<ArchivePathIdentity>,
    budget: Option<&ArchiveBudget>,
    cancellation: Option<&CancellationToken>,
    checkpoint: &mut dyn FnMut(
        JournalPhase,
        Option<ArchivePathIdentity>,
        ArchivePublicationPaths<'a>,
    ) -> Result<(), ArchiveOperationError>,
) -> Result<ArchiveOperationOutcome, ArchiveOperationError> {
    use rustix::fs::{Mode, OFlags, RenameFlags, open, openat, renameat_with};
    let staging = paths.source;
    let rollback = paths.rollback;
    let published_quarantine = paths.payload_quarantine;
    let parent_path = staging.parent().ok_or(ArchiveOperationError::UnsafePath(
        "archive staging needs a parent",
    ))?;
    if destination.parent() != Some(parent_path)
        || rollback.parent() != Some(parent_path)
        || paths.rollback_quarantine.parent() != Some(parent_path)
        || published_quarantine.parent() != Some(parent_path)
    {
        return Err(ArchiveOperationError::UnsafePath(
            "archive staging and destination must be siblings",
        ));
    }
    let staging_name = staging
        .file_name()
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive staging needs a file name",
        ))?;
    let destination_name = destination
        .file_name()
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive destination needs a file name",
        ))?;
    let rollback_name = rollback
        .file_name()
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive rollback location needs a file name",
        ))?;
    let published_quarantine_name =
        published_quarantine
            .file_name()
            .ok_or(ArchiveOperationError::UnsafePath(
                "archive publication quarantine needs a file name",
            ))?;
    let parent = open(
        parent_path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(map_errno)?;
    let staging_handle = openat(
        &parent,
        staging_name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(map_errno)?;
    let staging_metadata = File::from(staging_handle)
        .metadata()
        .map_err(|error| map_io(&error))?;
    if staging_metadata.dev() != expected_staging.device()
        || staging_metadata.ino() != expected_staging.inode()
        || path_identity_with_controls(staging, budget, cancellation)? != Some(expected_staging)
    {
        return Err(ArchiveOperationError::Conflict);
    }
    if path_identity_with_controls(destination, budget, cancellation)? != expected_destination {
        return Err(ArchiveOperationError::Conflict);
    }
    if path_identity_with_controls(rollback, budget, cancellation)?.is_some() {
        return Err(ArchiveOperationError::RecoveryRequired);
    }
    if published_quarantine != staging
        && path_identity_with_controls(published_quarantine, budget, cancellation)?.is_some()
    {
        return Err(ArchiveOperationError::RecoveryRequired);
    }
    let outcome = match (expected_destination, policy) {
        (Some(_), ArchiveConflictPolicy::Fail) => Err(ArchiveOperationError::Conflict),
        (Some(_), ArchiveConflictPolicy::Skip) => Ok(ArchiveOperationOutcome::Skipped),
        (Some(expected), ArchiveConflictPolicy::Replace) => {
            checkpoint(JournalPhase::DestinationQuarantinePlanned, None, paths)?;
            renameat_with(
                &parent,
                destination_name,
                &parent,
                rollback_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(map_errno)?;
            rustix::fs::fsync(&parent).map_err(map_errno)?;
            checkpoint(JournalPhase::DestinationQuarantined, None, paths)?;
            if path_identity_with_controls(rollback, budget, cancellation)? != Some(expected) {
                return Err(ArchiveOperationError::RecoveryRequired);
            }
            checkpoint(JournalPhase::StagePublishPlanned, None, paths)?;
            renameat_with(
                &parent,
                staging_name,
                &parent,
                destination_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(map_errno)?;
            rustix::fs::fsync(&parent).map_err(map_errno)?;
            Ok(ArchiveOperationOutcome::Published)
        }
        (None, _) => {
            checkpoint(JournalPhase::StagePublishPlanned, None, paths)?;
            match renameat_with(
                &parent,
                staging_name,
                &parent,
                destination_name,
                RenameFlags::NOREPLACE,
            ) {
                Ok(()) => {
                    rustix::fs::fsync(&parent).map_err(map_errno)?;
                    Ok(ArchiveOperationOutcome::Published)
                }
                Err(error) if error == rustix::io::Errno::EXIST => match policy {
                    ArchiveConflictPolicy::Skip => Ok(ArchiveOperationOutcome::Skipped),
                    ArchiveConflictPolicy::Fail | ArchiveConflictPolicy::Replace => {
                        Err(ArchiveOperationError::Conflict)
                    }
                },
                Err(error) => Err(map_errno(error)),
            }
        }
    }?;
    if outcome == ArchiveOperationOutcome::Published
        && (budget.is_some_and(ArchiveBudget::force_post_publish_failure)
            || !published_root_matches(&parent, destination_name, expected_staging)?
            || path_identity_with_controls(destination, budget, cancellation)?
                != Some(expected_staging))
    {
        if budget.is_some_and(ArchiveBudget::recreate_stage_during_rollback) {
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(staging)
                .and_then(|mut file| file.write_all(b"foreign stage replacement"));
        }
        if budget.is_some_and(|budget| budget.check_rollback().is_err()) {
            rustix::fs::fsync(&parent).map_err(map_errno)?;
            return Err(ArchiveOperationError::RecoveryRequired);
        }
        checkpoint(
            JournalPhase::PublishRollbackPlanned,
            Some(expected_staging),
            paths,
        )?;
        renameat_with(
            &parent,
            destination_name,
            &parent,
            published_quarantine_name,
            RenameFlags::NOREPLACE,
        )
        .map_err(|_| ArchiveOperationError::RecoveryRequired)?;
        rustix::fs::fsync(&parent).map_err(map_errno)?;
        checkpoint(JournalPhase::PublishedPayloadQuarantined, None, paths)?;
        checkpoint(JournalPhase::DestinationRestorePlanned, None, paths)?;
        if expected_destination.is_some() {
            renameat_with(
                &parent,
                rollback_name,
                &parent,
                destination_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(|_| ArchiveOperationError::RecoveryRequired)?;
        }
        rustix::fs::fsync(&parent).map_err(map_errno)?;
        if path_identity_with_controls(destination, budget, cancellation)? != expected_destination
            || path_identity_with_controls(published_quarantine, budget, cancellation)?
                != Some(expected_staging)
        {
            return Err(ArchiveOperationError::RecoveryRequired);
        }
        checkpoint(
            JournalPhase::DestinationRestored,
            expected_destination,
            paths,
        )?;
        return Err(ArchiveOperationError::RecoveryRequired);
    }
    if outcome == ArchiveOperationOutcome::Published {
        checkpoint(
            JournalPhase::DestinationPublished,
            Some(expected_staging),
            paths,
        )?;
    }
    Ok(outcome)
}

fn published_root_matches(
    parent: &impl std::os::fd::AsFd,
    name: &std::ffi::OsStr,
    expected: ArchivePathIdentity,
) -> Result<bool, ArchiveOperationError> {
    use rustix::fs::{Mode, OFlags, openat};
    let opened = match openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(opened) => opened,
        Err(rustix::io::Errno::NOENT) => return Ok(false),
        Err(error) => return Err(map_errno(error)),
    };
    let metadata = File::from(opened)
        .metadata()
        .map_err(|error| map_io(&error))?;
    Ok(metadata.dev() == expected.device() && metadata.ino() == expected.inode())
}

pub(crate) fn map_errno(error: rustix::io::Errno) -> ArchiveOperationError {
    map_io(&io::Error::from_raw_os_error(error.raw_os_error()))
}

pub(crate) fn remove_owned_journaled(
    path: &Path,
    quarantine: &Path,
    expected: Option<ArchivePathIdentity>,
    budget: &ArchiveBudget,
    cancellation: &CancellationToken,
    after_quarantine: &mut dyn FnMut() -> Result<(), ArchiveOperationError>,
) -> Result<(), ArchiveOperationError> {
    remove_owned_with_identity(
        path,
        Some(quarantine),
        expected,
        true,
        Some(budget),
        Some(cancellation),
        Some(after_quarantine),
    )
}

/// Removes an owned staging folder that may hold items the archive did not
/// write, such as the items a merge replaced. Ownership is proven by device
/// and inode; what the folder holds is not read first.
pub(crate) fn remove_owned_journaled_by_identity(
    path: &Path,
    quarantine: &Path,
    expected: ArchivePathIdentity,
    budget: &ArchiveBudget,
    cancellation: &CancellationToken,
    after_quarantine: &mut dyn FnMut() -> Result<(), ArchiveOperationError>,
) -> Result<(), ArchiveOperationError> {
    remove_owned_with_identity(
        path,
        Some(quarantine),
        Some(expected),
        false,
        Some(budget),
        Some(cancellation),
        Some(after_quarantine),
    )
}

fn remove_owned_with_identity(
    path: &Path,
    quarantine: Option<&Path>,
    expected: Option<ArchivePathIdentity>,
    require_exact_content: bool,
    budget: Option<&ArchiveBudget>,
    cancellation: Option<&CancellationToken>,
    mut after_quarantine: Option<&mut dyn FnMut() -> Result<(), ArchiveOperationError>>,
) -> Result<(), ArchiveOperationError> {
    use rustix::fs::{Mode, OFlags, RenameFlags, open, openat, renameat_with};
    if let Some(budget) = budget {
        budget.check_cleanup()?;
    }
    let deletion = quarantine.map_or_else(|| deletion_path(path), |path| Ok(path.to_path_buf()))?;
    if path == deletion || path.parent().is_none() || path.parent() != deletion.parent() {
        return Err(ArchiveOperationError::UnsafePath(
            "archive cleanup source and quarantine must be distinct siblings",
        ));
    }
    let path_absent = matches!(
        std::fs::symlink_metadata(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    );
    let deletion_absent = matches!(
        std::fs::symlink_metadata(&deletion),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    );
    if path_absent && deletion_absent {
        return Ok(());
    }
    let Some(expected) = expected else {
        return Err(ArchiveOperationError::UnsafePath(
            "unowned archive staging or deletion path exists",
        ));
    };
    let parent_path = path.parent().ok_or(ArchiveOperationError::UnsafePath(
        "archive staging needs a parent",
    ))?;
    let name = path.file_name().ok_or(ArchiveOperationError::UnsafePath(
        "archive staging needs a file name",
    ))?;
    let parent = open(
        parent_path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(map_errno)?;
    let quarantine_name = deletion
        .file_name()
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive deletion path needs a file name",
        ))?;
    match openat(
        &parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(opened) => {
            let opened_metadata = File::from(opened)
                .metadata()
                .map_err(|error| map_io(&error))?;
            if opened_metadata.dev() != expected.device()
                || opened_metadata.ino() != expected.inode()
            {
                return Err(ArchiveOperationError::UnsafePath(
                    "archive staging ownership changed before cleanup",
                ));
            }
            if require_exact_content
                && path_identity_with_controls(path, budget, cancellation)? != Some(expected)
            {
                return Err(ArchiveOperationError::UnsafePath(
                    "archive staging content changed before cleanup",
                ));
            }
            renameat_with(
                &parent,
                name,
                &parent,
                quarantine_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(map_errno)?;
            rustix::fs::fsync(&parent).map_err(map_errno)?;
        }
        Err(rustix::io::Errno::NOENT) => {}
        Err(error) => return Err(map_errno(error)),
    }
    if !published_root_matches(&parent, quarantine_name, expected)? {
        return Err(ArchiveOperationError::RecoveryRequired);
    }
    if require_exact_content
        && path_identity_with_controls(&deletion, budget, cancellation)? != Some(expected)
    {
        return Err(ArchiveOperationError::RecoveryRequired);
    }
    if let Some(after_quarantine) = &mut after_quarantine {
        after_quarantine()?;
    }
    let deletion = if expected.is_directory() {
        let quarantine = openat(
            &parent,
            quarantine_name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(map_errno);
        quarantine.and_then(|quarantine| {
            let quarantine = File::from(quarantine);
            remove_open_directory(&quarantine, 0)?;
            if !published_root_matches(&parent, quarantine_name, expected)? {
                return Err(ArchiveOperationError::RecoveryRequired);
            }
            rustix::fs::unlinkat(&parent, quarantine_name, rustix::fs::AtFlags::REMOVEDIR)
                .map_err(map_errno)
        })
    } else {
        rustix::fs::unlinkat(&parent, quarantine_name, rustix::fs::AtFlags::empty())
            .map_err(map_errno)
    };
    if deletion.is_err() {
        return Err(ArchiveOperationError::RecoveryRequired);
    }
    rustix::fs::fsync(&parent).map_err(map_errno)
}

pub(crate) fn deletion_path(path: &Path) -> Result<PathBuf, ArchiveOperationError> {
    let name = path.file_name().ok_or(ArchiveOperationError::UnsafePath(
        "archive cleanup path needs a file name",
    ))?;
    let mut deletion_name = name.to_os_string();
    deletion_name.push(".delete");
    Ok(path.with_file_name(deletion_name))
}

/// Removes everything inside `directory`, never entering another
/// filesystem: a folder mounted inside it stops the removal.
pub(crate) fn remove_open_directory(
    directory: &File,
    depth: usize,
) -> Result<(), ArchiveOperationError> {
    use rustix::fs::{AtFlags, Mode, OFlags, openat, unlinkat};
    const MAX_CLEANUP_DEPTH: usize = 4_096;
    if depth > MAX_CLEANUP_DEPTH {
        return Err(ArchiveOperationError::RecoveryRequired);
    }
    let device = directory.metadata().map_err(|error| map_io(&error))?.dev();
    let proc_path = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    let entries = std::fs::read_dir(&proc_path)
        .map_err(|error| map_io(&error))?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name())
                .map_err(|error| map_io(&error))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for name in entries {
        let anchored = proc_path.join(&name);
        let metadata = std::fs::symlink_metadata(&anchored).map_err(|error| map_io(&error))?;
        if metadata.is_dir() {
            let child = openat(
                directory,
                &name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(map_errno)?;
            let child = File::from(child);
            let opened = child.metadata().map_err(|error| map_io(&error))?;
            if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
                return Err(ArchiveOperationError::RecoveryRequired);
            }
            if opened.dev() != device {
                return Err(ArchiveOperationError::UnsafePath(
                    "a folder to remove holds a mounted filesystem",
                ));
            }
            remove_open_directory(&child, depth.saturating_add(1))?;
            let current = std::fs::symlink_metadata(&anchored).map_err(|error| map_io(&error))?;
            if current.dev() != opened.dev() || current.ino() != opened.ino() {
                return Err(ArchiveOperationError::RecoveryRequired);
            }
            unlinkat(directory, &name, AtFlags::REMOVEDIR).map_err(map_errno)?;
        } else {
            unlinkat(directory, &name, AtFlags::empty()).map_err(map_errno)?;
        }
    }
    directory.sync_all().map_err(|error| map_io(&error))
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
