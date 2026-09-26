use super::budget::{
    ArchiveBudget, ArchiveMemoryLease, ArchiveOperationAccounting, ArchiveOperationError,
    ArchiveOperationLimits, map_io,
};
use super::create::{
    ArchiveCleanupIntent, ArchiveOperationOutcome, ArchivePublicationPaths, append_archive_phase,
    cleanup_path, cleanup_phase, deletion_path, file_identity, local_path, map_errno,
    path_identity_with_controls, publication_phase, publication_quarantine_path, publish_staging,
    remove_open_directory, remove_owned_journaled, staging_path, sync_parent,
};
use super::format::{ArchiveCopyContext, RawEntryKind, copy_entry, open_scanner};
use super::store::{ArchiveError, ArchiveLimits, ArchivePasswordProvider, DecodeCounterState};
use super::{ArchiveFormat, ArchivePath};
use super::{tar_codec, zip_codec};
use musheen_core::{CancellationToken, ProviderId};
use musheen_ops::{
    ArchiveCleanupKind, ArchiveCodec, ArchiveConflictPolicy, ArchiveEventPhase,
    ArchiveOperationPlan, EventGeneration, ExtractMerge, JobId, Journal, JournalPhase,
    JournalStorage,
};
use nix::libc::O_NOFOLLOW;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

struct ExtractEntry {
    path: Vec<u8>,
    kind: ExtractEntryKind,
    compressed_size: Option<u64>,
    ordinal: u64,
    _memory: ArchiveMemoryLease,
}

#[derive(Clone, Copy)]
enum ExtractEntryKind {
    Directory,
    File,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_extract<S: JournalStorage>(
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
    let source = local_path(
        plan.sources()
            .first()
            .ok_or(ArchiveOperationError::InvalidArchive)?,
    )?;
    let destination = local_path(plan.destination())?;
    // A merge needs a real folder at the destination; anything else follows
    // the plan's conflict policy for the destination as a whole.
    let merge = match (plan.merge(), std::fs::symlink_metadata(&destination)) {
        (Some(merge), Ok(metadata)) if metadata.is_dir() => Some(merge),
        _ => None,
    };
    if merge.is_none() && destination.exists() {
        match plan.conflict_policy() {
            ArchiveConflictPolicy::Fail => return Err(ArchiveOperationError::Conflict),
            ArchiveConflictPolicy::Skip => return Ok(ArchiveOperationOutcome::Skipped),
            ArchiveConflictPolicy::Replace => {}
        }
    }
    let snapshot_parent = destination
        .parent()
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive destination needs a parent",
        ))?;
    let operation_limits = limits.for_staging(snapshot_parent)?;
    let mut budget = ArchiveBudget::with_accounting(operation_limits.clone(), accounting.clone())
        .with_identity_cancellation(cancellation.clone());
    let plan_path_bytes = plan
        .sources()
        .iter()
        .chain(std::iter::once(plan.destination()))
        .map(|path| {
            path.as_unix_path()
                .map_or(0, |value| value.as_os_str().len())
        })
        .sum::<usize>();
    let _plan_paths_memory =
        budget.reserve_memory(u64::try_from(plan_path_bytes).unwrap_or(u64::MAX))?;
    let counters = DecodeCounterState::with_memory_budget(budget.shared_memory());
    let source_file = open_archive_source(&source)?;
    let source_identity = file_identity(&source_file)?;
    let source_metadata = source_file.metadata().map_err(|error| map_io(&error))?;
    let source_bytes = source_metadata.len();
    budget.charge_temporary(source_bytes)?;
    let mut source_snapshot =
        tempfile::tempfile_in(snapshot_parent).map_err(|error| map_io(&error))?;
    copy_source_snapshot(
        &source_file,
        &mut source_snapshot,
        source_bytes,
        source_metadata.mode(),
        source_identity.content_digest(),
        cancellation,
        &budget,
    )?;
    if path_identity_with_controls(&source, Some(&budget), Some(cancellation))?
        != Some(source_identity)
        || file_identity(&source_file)? != source_identity
    {
        return Err(ArchiveOperationError::Conflict);
    }
    source_snapshot
        .seek(SeekFrom::Start(0))
        .map_err(|error| map_io(&error))?;
    let format = archive_format(plan.codec());
    let decode_limits = decode_limits(&operation_limits);
    let entries = collect_extract_entries(
        &source_snapshot,
        source_bytes,
        format,
        &decode_limits,
        passwords,
        cancellation,
        &mut budget,
        &counters,
        destination
            .parent()
            .ok_or(ArchiveOperationError::UnsafePath(
                "archive destination needs a parent",
            ))?,
    )?;
    let staging = staging_path(plan, job_id, generation)?;
    let rollback = cleanup_path(&staging)?;
    let destination_before =
        path_identity_with_controls(&destination, Some(&budget), Some(cancellation))?;
    append_archive_phase(
        journal,
        job_id,
        generation,
        JournalPhase::Planned,
        plan,
        &staging,
        destination_before,
        None,
        Some(&budget),
    )?;
    let mut published = false;
    let mut transaction_started = false;
    let mut stage_root = None;
    let mut result = (|| {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&staging)
            .map_err(|error| map_io(&error))?;
        sync_parent(&staging)?;
        stage_root = path_identity_with_controls(&staging, Some(&budget), Some(cancellation))?;
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
            Some(&budget),
        )?;
        report_phase(ArchiveEventPhase::Decoding)?;
        let mut actual_budget = budget.next_phase();
        for entry in &entries {
            cancellation.wait_if_paused()?;
            let _path_memory = actual_budget.reserve_memory(
                u64::try_from(entry.path.len().saturating_add(256)).unwrap_or(u64::MAX),
            )?;
            actual_budget.charge_temporary(4 * 1_024)?;
            let output = output_path(&staging, &entry.path, limits.max_path_bytes)?;
            match entry.kind {
                ExtractEntryKind::Directory => {
                    std::fs::create_dir_all(&output).map_err(|error| map_io(&error))?;
                    std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o700))
                        .map_err(|error| map_io(&error))?;
                }
                ExtractEntryKind::File => {
                    let parent = output.parent().ok_or(ArchiveOperationError::UnsafePath(
                        "archive entry needs a parent",
                    ))?;
                    std::fs::create_dir_all(parent).map_err(|error| map_io(&error))?;
                    let file = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .custom_flags(O_NOFOLLOW)
                        .mode(0o600)
                        .open(&output)
                        .map_err(|error| map_io(&error))?;
                    let mut writer = BudgetWriter {
                        inner: file,
                        budget: &mut actual_budget,
                        source_bytes,
                        error: None,
                    };
                    let copy_result = copy_entry(
                        &source_snapshot,
                        format,
                        entry.ordinal,
                        &mut writer,
                        ArchiveCopyContext {
                            passwords,
                            limits: &decode_limits,
                            counters: &counters,
                            cancellation,
                            compressed_size: entry.compressed_size,
                        },
                    );
                    if let Some(error) = writer.error.take() {
                        return Err(error);
                    }
                    copy_result.map_err(ArchiveOperationError::from)?;
                    writer.inner.sync_all().map_err(|error| map_io(&error))?;
                }
            }
        }
        sync_tree(&staging)?;
        append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::DataCopied,
            plan,
            &staging,
            destination_before,
            None,
            Some(&budget),
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
            Some(&budget),
        )?;
        report_phase(ArchiveEventPhase::Publishing)?;
        cancellation.wait_if_paused()?;
        let staging_before_publish =
            path_identity_with_controls(&staging, Some(&budget), Some(cancellation))?
                .ok_or(ArchiveOperationError::Conflict)?;
        begin_commit()?;
        transaction_started = true;
        if let Some(merge) = merge {
            merge_staging(&staging, &destination, merge, cancellation)?;
            published = true;
            sync_parent(&destination)?;
            let destination_after =
                path_identity_with_controls(&destination, Some(&budget), Some(cancellation))?;
            // What the merge skipped is still staged; remove it.
            if let Some(leftover) =
                path_identity_with_controls(&staging, Some(&budget), Some(cancellation))?
            {
                let owned = stage_root.is_some_and(|created| {
                    created.device() == leftover.device() && created.inode() == leftover.inode()
                });
                if !owned {
                    return Err(ArchiveOperationError::RecoveryRequired);
                }
                let stage_deletion = deletion_path(&staging)?;
                let cleanup = ArchiveCleanupIntent::new(
                    ArchiveCleanupKind::PrepublishStage,
                    &staging,
                    &stage_deletion,
                    Some(leftover),
                );
                append_archive_phase(
                    journal,
                    job_id,
                    generation,
                    cleanup_phase(JournalPhase::PrepublishStageCleanupPlanned, cleanup),
                    plan,
                    &staging,
                    destination_before,
                    destination_after,
                    Some(&budget),
                )?;
                let mut quarantined = || {
                    append_archive_phase(
                        journal,
                        job_id,
                        generation,
                        cleanup_phase(JournalPhase::PrepublishStageCleanupQuarantined, cleanup),
                        plan,
                        &staging,
                        destination_before,
                        destination_after,
                        Some(&budget),
                    )
                };
                remove_owned_journaled(
                    &staging,
                    &stage_deletion,
                    Some(leftover),
                    &budget,
                    cancellation,
                    &mut quarantined,
                )
                .map_err(|_| ArchiveOperationError::RecoveryRequired)?;
            }
            for phase in [JournalPhase::StagingCleaned, JournalPhase::Completed] {
                append_archive_phase(
                    journal,
                    job_id,
                    generation,
                    phase,
                    plan,
                    &staging,
                    destination_before,
                    destination_after,
                    Some(&budget),
                )?;
            }
            return Ok(ArchiveOperationOutcome::Published);
        }
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
                    Some(&budget),
                )
            };
            publish_staging(
                publication,
                &destination,
                plan.conflict_policy(),
                staging_before_publish,
                destination_before,
                Some(&budget),
                Some(cancellation),
                &mut publication_checkpoint,
            )?
        };
        if outcome == ArchiveOperationOutcome::Skipped {
            if path_identity_with_controls(&staging, Some(&budget), Some(cancellation))?
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
                Some(&budget),
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
                        Some(&budget),
                    )
                };
                remove_owned_journaled(
                    &staging,
                    &stage_deletion,
                    Some(staging_before_publish),
                    &budget,
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
                Some(&budget),
            )?;
            return Ok(outcome);
        }
        published = true;
        sync_parent(&destination)?;
        let destination_after =
            path_identity_with_controls(&destination, Some(&budget), Some(cancellation))?;
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
            Some(&budget),
        )?;
        report_phase(ArchiveEventPhase::Cleaning)?;
        if path_identity_with_controls(&rollback, Some(&budget), Some(cancellation))?
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
                    Some(&budget),
                )
            };
            remove_owned_journaled(
                &rollback,
                &rollback_deletion,
                destination_before,
                &budget,
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
            Some(&budget),
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
            Some(&budget),
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

/// Moves staged entries into the existing destination folder. An entry the
/// folder lacks moves in whole; folders on both sides merge entry by entry;
/// any other collision is replaced only when `merge` names its path, and
/// otherwise stays staged for cleanup. Every step works relative to opened
/// folders, never follows a symlink in the destination, and never replaces
/// an item that appeared after the user answered.
fn merge_staging(
    staging: &Path,
    destination: &Path,
    merge: &ExtractMerge,
    cancellation: &CancellationToken,
) -> Result<(), ArchiveOperationError> {
    let staged = open_directory(staging)?;
    let target = open_directory(destination)?;
    merge_directory(&staged, &target, &mut Vec::new(), merge, cancellation, 0)
}

fn open_directory(path: &Path) -> Result<File, ArchiveOperationError> {
    use rustix::fs::{Mode, OFlags, open};
    open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(map_errno)
}

fn open_child_directory(
    parent: &File,
    name: &std::ffi::CStr,
) -> Result<File, ArchiveOperationError> {
    use rustix::fs::{Mode, OFlags, openat};
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(map_errno)
}

fn merge_directory(
    staged: &File,
    target: &File,
    relative: &mut Vec<u8>,
    merge: &ExtractMerge,
    cancellation: &CancellationToken,
    depth: usize,
) -> Result<(), ArchiveOperationError> {
    use rustix::fs::{AtFlags, Dir, FileType, RenameFlags, renameat_with, statat, unlinkat};
    use rustix::io::Errno;
    const MAX_MERGE_DEPTH: usize = 4_096;
    if depth > MAX_MERGE_DEPTH {
        return Err(ArchiveOperationError::UnsafePath(
            "archive folders nest too deep",
        ));
    }
    let mut names = Vec::new();
    for entry in Dir::read_from(staged).map_err(map_errno)? {
        let entry = entry.map_err(map_errno)?;
        let name = entry.file_name();
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            names.push(name.to_owned());
        }
    }
    names.sort();
    let is_directory = |mode| FileType::from_raw_mode(mode) == FileType::Directory;
    for name in names {
        cancellation.wait_if_paused()?;
        let mark = relative.len();
        if !relative.is_empty() {
            relative.push(b'/');
        }
        relative.extend_from_slice(name.to_bytes());
        let staged_is_directory = is_directory(
            statat(staged, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(map_errno)?
                .st_mode,
        );
        let moved = match statat(target, &name, AtFlags::SYMLINK_NOFOLLOW) {
            Err(Errno::NOENT) => {
                renameat_with(staged, &name, target, &name, RenameFlags::NOREPLACE)
            }
            Err(error) => return Err(map_errno(error)),
            Ok(existing) if is_directory(existing.st_mode) && staged_is_directory => {
                let staged_child = open_child_directory(staged, &name)?;
                let target_child = open_child_directory(target, &name)?;
                merge_directory(
                    &staged_child,
                    &target_child,
                    relative,
                    merge,
                    cancellation,
                    depth.saturating_add(1),
                )?;
                Ok(())
            }
            Ok(_) if !merge.replaces(relative) => Ok(()),
            Ok(existing) if is_directory(existing.st_mode) => {
                let existing_child = open_child_directory(target, &name)?;
                remove_open_directory(&existing_child, 0)?;
                unlinkat(target, &name, AtFlags::REMOVEDIR).map_err(map_errno)?;
                renameat_with(staged, &name, target, &name, RenameFlags::NOREPLACE)
            }
            Ok(_) if staged_is_directory => {
                unlinkat(target, &name, AtFlags::empty()).map_err(map_errno)?;
                renameat_with(staged, &name, target, &name, RenameFlags::NOREPLACE)
            }
            // A file over a file or a link: one rename replaces it in place.
            Ok(_) => renameat_with(staged, &name, target, &name, RenameFlags::empty()),
        };
        match moved {
            // An item that appeared after the question stays; the staged
            // entry is cleaned up with the rest of the staging.
            Ok(()) | Err(Errno::EXIST) => {}
            Err(error) => return Err(map_errno(error)),
        }
        relative.truncate(mark);
    }
    target.sync_all().map_err(|error| map_io(&error))
}

/// What an extraction would meet at its destination folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtractDestination {
    /// The folder does not exist; the extraction publishes it in one step.
    Absent,
    /// An item other than a folder has the folder's name.
    NotAFolder,
    /// The folder exists; the items that collide with archive entries.
    Folder(Vec<ExtractCollision>),
}

/// An existing item an archive entry would replace, by its path relative to
/// the destination folder with `/` between components.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractCollision {
    pub path: Vec<u8>,
    pub existing_is_folder: bool,
}

/// Lists the collisions an extraction would meet, reading the archive's
/// entries as the extraction itself does and never following a symlink in
/// the destination. A folder that meets a folder merges and is no collision;
/// the entries under a colliding item follow that item's answer.
pub fn extract_destination(
    plan: &ArchiveOperationPlan,
    limits: &ArchiveOperationLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
) -> Result<ExtractDestination, ArchiveOperationError> {
    let destination = local_path(plan.destination())?;
    match std::fs::symlink_metadata(&destination) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ExtractDestination::Absent);
        }
        Err(error) => return Err(map_io(&error)),
        Ok(metadata) if !metadata.is_dir() => return Ok(ExtractDestination::NotAFolder),
        Ok(_) => {}
    }
    let source = local_path(
        plan.sources()
            .first()
            .ok_or(ArchiveOperationError::InvalidArchive)?,
    )?;
    let parent = destination
        .parent()
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive destination needs a parent",
        ))?;
    let operation_limits = limits.for_staging(parent)?;
    let mut budget = ArchiveBudget::new(operation_limits.clone());
    let counters = DecodeCounterState::with_memory_budget(budget.shared_memory());
    let source_file = open_archive_source(&source)?;
    let source_bytes = source_file
        .metadata()
        .map_err(|error| map_io(&error))?
        .len();
    let entries = collect_extract_entries(
        &source_file,
        source_bytes,
        archive_format(plan.codec()),
        &decode_limits(&operation_limits),
        passwords,
        cancellation,
        &mut budget,
        &counters,
        parent,
    )?;
    let mut collisions: Vec<ExtractCollision> = Vec::new();
    for entry in &entries {
        let relative = ArchivePath::normalize_bytes(&entry.path, limits.max_path_bytes)?;
        let components = relative.split(|byte| *byte == b'/').collect::<Vec<_>>();
        let mut prefix = Vec::new();
        for (index, component) in components.iter().enumerate() {
            if !prefix.is_empty() {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(component);
            if collisions.iter().any(|collision| collision.path == prefix) {
                break;
            }
            let entry_is_folder =
                index + 1 < components.len() || matches!(entry.kind, ExtractEntryKind::Directory);
            let existing = match std::fs::symlink_metadata(
                destination.join(OsString::from_vec(prefix.clone())),
            ) {
                Ok(existing) => existing,
                Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                Err(error) => return Err(map_io(&error)),
            };
            if existing.is_dir() && entry_is_folder {
                continue;
            }
            collisions.push(ExtractCollision {
                path: prefix.clone(),
                existing_is_folder: existing.is_dir(),
            });
            break;
        }
    }
    Ok(ExtractDestination::Folder(collisions))
}

fn copy_source_snapshot(
    source: &File,
    snapshot: &mut File,
    source_bytes: u64,
    source_mode: u32,
    expected_digest: [u8; 32],
    cancellation: &CancellationToken,
    budget: &ArchiveBudget,
) -> Result<(), ArchiveOperationError> {
    let _memory = budget.reserve_memory(8 * 1_024)?;
    let mut reader = source.try_clone().map_err(|error| map_io(&error))?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| map_io(&error))?;
    let mut copied = 0_u64;
    let mut digest = blake3::Hasher::new();
    digest.update(&source_mode.to_le_bytes());
    digest.update(&source_bytes.to_le_bytes());
    let mut buffer = [0_u8; 8 * 1_024];
    loop {
        cancellation.wait_if_paused()?;
        let count = reader.read(&mut buffer).map_err(|error| map_io(&error))?;
        if count == 0 {
            break;
        }
        snapshot
            .write_all(&buffer[..count])
            .map_err(|error| map_io(&error))?;
        digest.update(&buffer[..count]);
        copied = copied.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
    }
    if copied != source_bytes || *digest.finalize().as_bytes() != expected_digest {
        return Err(ArchiveOperationError::Conflict);
    }
    snapshot.sync_all().map_err(|error| map_io(&error))
}

#[allow(clippy::too_many_arguments)]
fn collect_extract_entries(
    source: &File,
    source_bytes: u64,
    format: ArchiveFormat,
    limits: &ArchiveLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
    budget: &mut ArchiveBudget,
    counters: &std::sync::Arc<DecodeCounterState>,
    temporary_root: &Path,
) -> Result<Vec<ExtractEntry>, ArchiveOperationError> {
    let provider = ProviderId::new("archive-extract").map_err(|_| ArchiveOperationError::Io)?;
    let mut scanner = open_scanner(source, provider, format, limits, counters, passwords)?;
    let mut entries = Vec::new();
    let mut total_expanded = 0_u64;
    let mut total_compressed = 0_u64;
    let mut all_file_sizes_known = true;
    while let Some(raw) = scanner.next_entry(cancellation)? {
        budget.charge_entry()?;
        budget.check_path(&raw.path)?;
        let memory = budget.reserve_memory(
            u64::try_from(
                raw.path
                    .len()
                    .saturating_add(std::mem::size_of::<ExtractEntry>()),
            )
            .unwrap_or(u64::MAX),
        )?;
        entries
            .try_reserve_exact(1)
            .map_err(|_| ArchiveOperationError::Io)?;
        let kind = match raw.kind {
            RawEntryKind::Directory => ExtractEntryKind::Directory,
            RawEntryKind::RegularFile => ExtractEntryKind::File,
            RawEntryKind::SymbolicLink | RawEntryKind::HardLink | RawEntryKind::Other => {
                return Err(ArchiveOperationError::UnsupportedFileType);
            }
        };
        let size = raw.size.unwrap_or(0);
        total_expanded =
            total_expanded
                .checked_add(size)
                .ok_or(ArchiveOperationError::LimitExceeded {
                    resource: "expanded bytes",
                    value: u64::MAX,
                    maximum: limits.max_expanded_bytes,
                })?;
        budget.check_expanded(total_expanded)?;
        if matches!(kind, ExtractEntryKind::File) {
            if let Some(compressed_size) = raw.compressed_size {
                total_compressed = total_compressed.checked_add(compressed_size).ok_or(
                    ArchiveOperationError::LimitExceeded {
                        resource: "compressed bytes",
                        value: u64::MAX,
                        maximum: u64::MAX,
                    },
                )?;
            } else {
                all_file_sizes_known = false;
            }
        }
        if matches!(kind, ExtractEntryKind::File) {
            inspect_nested_entry(
                source,
                format,
                raw.ordinal,
                raw.compressed_size,
                limits,
                passwords,
                cancellation,
                budget,
                counters,
                temporary_root,
                1,
            )?;
        }
        entries.push(ExtractEntry {
            path: raw.path,
            kind,
            compressed_size: raw.compressed_size,
            ordinal: raw.ordinal,
            _memory: memory,
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    if entries.windows(2).any(|pair| pair[0].path == pair[1].path) {
        return Err(ArchiveOperationError::InvalidArchive);
    }
    let compressed_budget = if all_file_sizes_known {
        total_compressed.max(1)
    } else {
        source_bytes.max(1)
    };
    budget.charge_expanded(total_expanded, compressed_budget)?;
    Ok(entries)
}

#[allow(clippy::too_many_arguments)]
fn inspect_nested_entry(
    source: &File,
    format: ArchiveFormat,
    ordinal: u64,
    compressed_size: Option<u64>,
    limits: &ArchiveLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
    budget: &mut ArchiveBudget,
    counters: &std::sync::Arc<DecodeCounterState>,
    temporary_root: &Path,
    depth: usize,
) -> Result<(), ArchiveOperationError> {
    let temporary =
        tempfile::NamedTempFile::new_in(temporary_root).map_err(|error| map_io(&error))?;
    let error = std::rc::Rc::new(std::cell::RefCell::new(None));
    let mut writer = NestedBudgetWriter {
        inner: temporary.reopen().map_err(|error| map_io(&error))?,
        budget,
        error: std::rc::Rc::clone(&error),
    };
    let copy_result = copy_entry(
        source,
        format,
        ordinal,
        &mut writer,
        ArchiveCopyContext {
            passwords,
            limits,
            counters,
            cancellation,
            compressed_size,
        },
    );
    if let Some(error) = error.borrow_mut().take() {
        return Err(error);
    }
    copy_result.map_err(ArchiveOperationError::from)?;
    writer.inner.flush().map_err(|error| map_io(&error))?;
    let NestedBudgetWriter { mut inner, .. } = writer;
    inner
        .seek(SeekFrom::Start(0))
        .map_err(|error| map_io(&error))?;
    let Some(nested_format) = detect_archive_format(&mut inner, limits, counters)? else {
        return Ok(());
    };
    budget.check_nesting(depth)?;
    scan_nested_archive(
        &inner,
        nested_format,
        limits,
        passwords,
        cancellation,
        budget,
        counters,
        temporary_root,
        depth,
    )
}

#[allow(clippy::too_many_arguments)]
fn scan_nested_archive(
    source: &File,
    format: ArchiveFormat,
    limits: &ArchiveLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
    budget: &mut ArchiveBudget,
    counters: &std::sync::Arc<DecodeCounterState>,
    temporary_root: &Path,
    depth: usize,
) -> Result<(), ArchiveOperationError> {
    let provider = ProviderId::new("archive-nested").map_err(|_| ArchiveOperationError::Io)?;
    let mut scanner = open_scanner(source, provider, format, limits, counters, passwords)?;
    let source_bytes = source
        .metadata()
        .map_err(|error| map_io(&error))?
        .len()
        .max(1);
    let mut expanded = 0_u64;
    let mut compressed = 0_u64;
    while let Some(raw) = scanner.next_entry(cancellation)? {
        budget.charge_entry()?;
        budget.check_path(&raw.path)?;
        match raw.kind {
            RawEntryKind::Directory => {}
            RawEntryKind::RegularFile => {
                let size = raw.size.unwrap_or(0);
                expanded = expanded.saturating_add(size);
                compressed = compressed.saturating_add(raw.compressed_size.unwrap_or(0));
                inspect_nested_entry(
                    source,
                    format,
                    raw.ordinal,
                    raw.compressed_size,
                    limits,
                    passwords,
                    cancellation,
                    budget,
                    counters,
                    temporary_root,
                    depth.saturating_add(1),
                )?;
            }
            RawEntryKind::SymbolicLink | RawEntryKind::HardLink | RawEntryKind::Other => {
                return Err(ArchiveOperationError::UnsupportedFileType);
            }
        }
    }
    budget.check_compression_ratio(expanded, compressed.max(source_bytes))?;
    budget.charge_expanded_bytes(expanded)
}

fn detect_archive_format(
    file: &mut File,
    limits: &ArchiveLimits,
    counters: &std::sync::Arc<DecodeCounterState>,
) -> Result<Option<ArchiveFormat>, ArchiveOperationError> {
    match zip_codec::preflight(file, limits, counters) {
        Ok(_) => {
            file.seek(SeekFrom::Start(0))
                .map_err(|error| map_io(&error))?;
            return Ok(Some(ArchiveFormat::Zip));
        }
        Err(ArchiveError::InvalidArchive) => {}
        Err(error) => return Err(error.into()),
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| map_io(&error))?;
    let mut prefix = [0_u8; 1_024];
    let count = file.read(&mut prefix).map_err(|error| map_io(&error))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| map_io(&error))?;
    let bytes = &prefix[..count];
    let format = if bytes.starts_with(b"7z\xBC\xAF\x27\x1C") {
        Some(ArchiveFormat::SevenZip)
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        Some(ArchiveFormat::TarGzip)
    } else if bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
        || bytes.get(..4).is_some_and(|magic| {
            (0x184d_2a50..=0x184d_2a5f).contains(&u32::from_le_bytes([
                magic[0], magic[1], magic[2], magic[3],
            ]))
        })
    {
        Some(ArchiveFormat::TarZstd)
    } else if count == prefix.len()
        && (prefix.iter().all(|byte| *byte == 0)
            || tar_codec::valid_tar_checksum(
                prefix[..512]
                    .try_into()
                    .map_err(|_| ArchiveOperationError::InvalidArchive)?,
            ))
    {
        Some(ArchiveFormat::Tar)
    } else {
        None
    };
    Ok(format)
}

struct NestedBudgetWriter<'a> {
    inner: File,
    budget: &'a mut ArchiveBudget,
    error: std::rc::Rc<std::cell::RefCell<Option<ArchiveOperationError>>>,
}

impl Write for NestedBudgetWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if let Err(error) = self.budget.charge_temporary(count) {
            *self.error.borrow_mut() = Some(error);
            return Err(io::Error::other(
                "nested archive temporary-space budget exceeded",
            ));
        }
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn output_path(
    root: &Path,
    raw: &[u8],
    max_path_bytes: usize,
) -> Result<PathBuf, ArchiveOperationError> {
    let normalized = ArchivePath::normalize_bytes(raw, max_path_bytes)?;
    if normalized.is_empty() {
        return Err(ArchiveOperationError::UnsafePath(
            "archive entries need a file name",
        ));
    }
    let relative = PathBuf::from(OsString::from_vec(normalized));
    let output = root.join(relative);
    if !output.starts_with(root) {
        return Err(ArchiveOperationError::UnsafePath(
            "archive entry escaped the staging root",
        ));
    }
    Ok(output)
}

fn open_archive_source(path: &Path) -> Result<File, ArchiveOperationError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(path)
        .map_err(|error| map_io(&error))?;
    if !file.metadata().map_err(|error| map_io(&error))?.is_file() {
        return Err(ArchiveOperationError::UnsupportedFileType);
    }
    Ok(file)
}

fn archive_format(codec: ArchiveCodec) -> ArchiveFormat {
    match codec {
        ArchiveCodec::Zip => ArchiveFormat::Zip,
        ArchiveCodec::Tar => ArchiveFormat::Tar,
        ArchiveCodec::TarGzip => ArchiveFormat::TarGzip,
        ArchiveCodec::TarZstd => ArchiveFormat::TarZstd,
        ArchiveCodec::SevenZip => ArchiveFormat::SevenZip,
    }
}

fn decode_limits(limits: &ArchiveOperationLimits) -> ArchiveLimits {
    ArchiveLimits {
        max_entries: usize::try_from(limits.max_entries).unwrap_or(usize::MAX),
        max_path_bytes: limits.max_path_bytes,
        max_metadata_bytes: usize::try_from(limits.max_memory_bytes).unwrap_or(usize::MAX),
        max_expanded_bytes: limits.max_expanded_bytes,
        max_compression_ratio: limits.max_compression_ratio,
        max_elapsed: Duration::from_secs(30),
        max_nested_archives: limits.max_nesting,
        max_nested_archive_bytes: limits.max_temporary_bytes,
    }
}

struct BudgetWriter<'a> {
    inner: File,
    budget: &'a mut ArchiveBudget,
    source_bytes: u64,
    error: Option<ArchiveOperationError>,
}

impl Write for BudgetWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if let Err(error) = self.budget.check_stage_write(count) {
            self.error = Some(map_io(&error));
            return Err(error);
        }
        if let Err(error) = self
            .budget
            .charge_expanded(count, self.source_bytes.max(1))
            .and_then(|()| self.budget.charge_temporary(count))
        {
            self.error = Some(error);
            return Err(io::Error::other("archive operation budget exceeded"));
        }
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn sync_tree(root: &Path) -> Result<(), ArchiveOperationError> {
    let mut directories = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.map_err(|_| ArchiveOperationError::Io)?;
        if entry.file_type().is_dir() {
            directories.push(entry.path().to_path_buf());
        }
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|error| map_io(&error))?;
    }
    Ok(())
}
